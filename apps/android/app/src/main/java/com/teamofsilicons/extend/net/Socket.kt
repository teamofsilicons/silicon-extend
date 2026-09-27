package com.teamofsilicons.extend.net

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/** What happened on a WebSocket, delivered in order on [Connection.events]. */
sealed interface SocketEvent {
    data object Open : SocketEvent
    data class Text(val text: String) : SocketEvent
    /** The peer closed (or started closing) with [code]. */
    data class Closed(val code: Int, val reason: String) : SocketEvent
    /**
     * The connection failed. [httpStatus] is set when the upgrade itself was refused; [refusal] is
     * the service's error from that answer (its code, message and, for 429, how long to wait).
     */
    data class Failed(val error: Throwable, val httpStatus: Int?, val refusal: ApiException? = null) : SocketEvent
}

/** One WebSocket connection as the app uses it; [Socket] is the real one, tests use fakes. */
interface Connection {
    val events: ReceiveChannel<SocketEvent>
    fun send(text: String): Boolean
    fun close(code: Int = 1000, reason: String = "")
}

/** A thin coroutine-friendly wrapper over an OkHttp WebSocket. */
class Socket private constructor() : Connection {
    override val events = Channel<SocketEvent>(Channel.UNLIMITED)
    private lateinit var ws: WebSocket
    @Volatile private var finished = false

    override fun send(text: String): Boolean = !finished && ws.send(text)

    override fun close(code: Int, reason: String) {
        finished = true
        runCatching { ws.close(code, reason) }
        runCatching { ws.cancel() }
    }

    companion object {
        fun open(client: OkHttpClient, url: String, authorization: String): Socket {
            val socket = Socket()
            val request = Request.Builder().url(url).header("Authorization", authorization).build()
            socket.ws = client.newWebSocket(request, object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    socket.events.trySend(SocketEvent.Open)
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    socket.events.trySend(SocketEvent.Text(text))
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    socket.finished = true
                    socket.events.trySend(SocketEvent.Closed(code, reason))
                    webSocket.close(1000, null)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    socket.finished = true
                    socket.events.trySend(SocketEvent.Closed(code, reason))
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    socket.finished = true
                    // A refused upgrade carries the service's error body (at most 64 KiB is read).
                    val refusal = response?.let { r ->
                        val body = runCatching { r.peekBody(64 * 1024).string() }.getOrDefault("")
                        ExtendApi.errorOf(r.code, body, r.header("Retry-After"), "GET ${r.request.url.encodedPath}")
                    }
                    socket.events.trySend(SocketEvent.Failed(t, response?.code, refusal))
                }
            })
            return socket
        }
    }
}
