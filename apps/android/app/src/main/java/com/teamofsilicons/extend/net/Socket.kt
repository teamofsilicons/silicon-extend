package com.teamofsilicons.extend.net

import kotlinx.coroutines.channels.Channel
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/** What happened on a WebSocket, delivered in order on [Socket.events]. */
sealed interface SocketEvent {
    data object Open : SocketEvent
    data class Text(val text: String) : SocketEvent
    /** The peer closed (or started closing) with [code]. */
    data class Closed(val code: Int, val reason: String) : SocketEvent
    /** The connection failed. [httpStatus] is set when the upgrade itself was refused. */
    data class Failed(val error: Throwable, val httpStatus: Int?) : SocketEvent
}

/** A thin coroutine-friendly wrapper over an OkHttp WebSocket. */
class Socket private constructor() {
    val events = Channel<SocketEvent>(Channel.UNLIMITED)
    private lateinit var ws: WebSocket
    @Volatile private var finished = false

    fun send(text: String): Boolean = !finished && ws.send(text)

    fun close(code: Int = 1000, reason: String = "") {
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
                    socket.events.trySend(SocketEvent.Failed(t, response?.code))
                }
            })
            return socket
        }
    }
}
