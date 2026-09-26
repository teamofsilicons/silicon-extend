// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0
// Silicon Extend: regression tests for its changes to the transport, against a scripted peer.

package io.github.muntashirakon.adb;

import org.junit.Test;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.KeyPairGenerator;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

public class ExtendTransportTest {
    private static KeyPair keyPair() throws Exception {
        KeyPairGenerator generator = KeyPairGenerator.getInstance("RSA");
        generator.initialize(2048);
        java.security.KeyPair pair = generator.generateKeyPair();
        Certificate certificate = new Certificate("X.509") {
            @Override public byte[] getEncoded() { return new byte[0]; }
            @Override public void verify(PublicKey key) { }
            @Override public void verify(PublicKey key, String sigProvider) { }
            @Override public String toString() { return "test certificate"; }
            @Override public PublicKey getPublicKey() { return pair.getPublic(); }
        };
        return new KeyPair(pair.getPrivate(), certificate);
    }

    /** A scripted ADB peer on loopback: one accepted socket. */
    private static final class Peer implements AutoCloseable {
        final ServerSocket server = new ServerSocket(0, 1, InetAddress.getLoopbackAddress());
        Socket socket;
        InputStream in;
        OutputStream out;

        Peer() throws IOException { }

        int port() { return server.getLocalPort(); }

        void accept() throws IOException {
            socket = server.accept();
            socket.setSoTimeout(5000);
            in = socket.getInputStream();
            out = socket.getOutputStream();
        }

        AdbProtocol.Message read() throws IOException {
            return AdbProtocol.Message.parse(in, AdbProtocol.A_VERSION_MIN, 1024 * 1024);
        }

        void send(int command, int arg0, int arg1, byte[] data) throws IOException {
            out.write(AdbProtocol.generateMessage(command, arg0, arg1, data));
            out.flush();
        }

        void sendConnect() throws IOException {
            send(AdbProtocol.A_CNXN, AdbProtocol.A_VERSION_MIN, 4096, "device::\0".getBytes(StandardCharsets.UTF_8));
        }

        @Override public void close() throws IOException {
            if (socket != null) socket.close();
            server.close();
        }
    }

    private static Thread connectInBackground(AdbConnection connection, AtomicReference<Object> outcome) {
        Thread thread = new Thread(() -> {
            try {
                outcome.set(connection.connect(5, TimeUnit.SECONDS, false));
            } catch (Exception e) {
                outcome.set(e);
            }
        });
        thread.start();
        return thread;
    }

    @Test
    public void acknowledgementForAnAbandonedOpenClosesTheDaemonsStream() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            peer.accept();
            AtomicReference<Object> connected = new AtomicReference<>();
            Thread connecting = connectInBackground(connection, connected);
            assertEquals(AdbProtocol.A_CNXN, peer.read().command);
            peer.sendConnect();
            connecting.join(5000);
            assertEquals(Boolean.TRUE, connected.get());

            AtomicReference<Throwable> opened = new AtomicReference<>();
            Thread opener = new Thread(() -> {
                try {
                    connection.open("shell:sleep 30");
                } catch (Throwable t) {
                    opened.set(t);
                }
            });
            opener.start();
            AdbProtocol.Message open = peer.read();
            assertEquals(AdbProtocol.A_OPEN, open.command);
            int localId = open.arg0;
            // The command is cancelled before the daemon answers the OPEN.
            opener.interrupt();
            opener.join(5000);
            assertTrue("The interrupted open must fail", opened.get() instanceof InterruptedException);
            AdbProtocol.Message abandoned = peer.read();
            assertEquals(AdbProtocol.A_CLSE, abandoned.command);
            assertEquals(localId, abandoned.arg0);
            assertEquals("The opener cannot know the daemon's id yet", 0, abandoned.arg1);

            // The daemon's acknowledgement arrives afterwards: its end must be closed by its id.
            peer.send(AdbProtocol.A_OKAY, 77, localId, null);
            AdbProtocol.Message close = peer.read();
            assertEquals(AdbProtocol.A_CLSE, close.command);
            assertEquals(localId, close.arg0);
            assertEquals(77, close.arg1);
            connection.close();
        }
    }

    @Test
    public void requiredTlsRefusesAPlainTextPeer() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            connection.setRequireTls(true);
            peer.accept();
            AtomicReference<Object> outcome = new AtomicReference<>();
            Thread connecting = connectInBackground(connection, outcome);
            assertEquals(AdbProtocol.A_CNXN, peer.read().command);
            peer.sendConnect();
            connecting.join(5000);
            assertTrue("A plain-text CNXN must not count as a connection: " + outcome.get(), outcome.get() instanceof IOException);
            assertTrue(((IOException) outcome.get()).getMessage().contains("without starting TLS"));
            assertTrue(!connection.isConnectionEstablished());
            connection.close();
        }
    }

    @Test
    public void requiredTlsNeverSignsAnAuthenticationToken() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            connection.setRequireTls(true);
            peer.accept();
            AtomicReference<Object> outcome = new AtomicReference<>();
            Thread connecting = connectInBackground(connection, outcome);
            assertEquals(AdbProtocol.A_CNXN, peer.read().command);
            peer.send(AdbProtocol.A_AUTH, AdbProtocol.ADB_AUTH_TOKEN, 0, new byte[20]);
            connecting.join(5000);
            assertTrue(String.valueOf(outcome.get()), outcome.get() instanceof IOException);
            assertTrue(((IOException) outcome.get()).getMessage().contains("legacy RSA authentication"));
            try {
                AdbProtocol.Message reply = peer.read();
                fail("No signature may be sent for a relayed token, got 0x" + Integer.toHexString(reply.command));
            } catch (IOException expected) {
                // Nothing was sent back.
            }
            connection.close();
        }
    }

    @Test
    public void legacyPeersStillConnectWhenTlsIsNotRequired() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            peer.accept();
            AtomicReference<Object> outcome = new AtomicReference<>();
            Thread connecting = connectInBackground(connection, outcome);
            assertEquals(AdbProtocol.A_CNXN, peer.read().command);
            peer.sendConnect();
            connecting.join(5000);
            assertEquals(Boolean.TRUE, outcome.get());
            connection.close();
        }
    }

    /** Connects [connection] to [peer] and opens one stream the peer acknowledges as [remoteId]. */
    private static AdbStream connectAndOpen(AdbConnection connection, Peer peer, int remoteId) throws Exception {
        peer.accept();
        AtomicReference<Object> connected = new AtomicReference<>();
        Thread connecting = connectInBackground(connection, connected);
        assertEquals(AdbProtocol.A_CNXN, peer.read().command);
        peer.sendConnect();
        connecting.join(5000);
        assertEquals(Boolean.TRUE, connected.get());
        AtomicReference<Object> opened = new AtomicReference<>();
        Thread opener = new Thread(() -> {
            try {
                opened.set(connection.open("shell:yes"));
            } catch (Throwable t) {
                opened.set(t);
            }
        });
        opener.start();
        AdbProtocol.Message open = peer.read();
        assertEquals(AdbProtocol.A_OPEN, open.command);
        peer.send(AdbProtocol.A_OKAY, remoteId, open.arg0, null);
        opener.join(5000);
        assertTrue(String.valueOf(opened.get()), opened.get() instanceof AdbStream);
        return (AdbStream) opened.get();
    }

    /** Nothing arrives from the connection within [ms]. */
    private static void assertSilent(Peer peer, int ms) throws IOException {
        peer.socket.setSoTimeout(ms);
        try {
            AdbProtocol.Message message = peer.read();
            fail("Expected no packet, got 0x" + Integer.toHexString(message.command));
        } catch (java.net.SocketTimeoutException expected) {
            // Nothing was sent.
        } finally {
            peer.socket.setSoTimeout(5000);
        }
    }

    /**
     * Flow control: the daemon sends the next WRTE only after our OKAY. Acknowledging on arrival
     * let a fast command (`head -c 270000000 /dev/zero`) queue its whole output in memory and
     * crash the app; the OKAY must wait until the reader takes the payload.
     */
    @Test
    public void dataIsAcknowledgedOnlyWhenTheReaderTakesIt() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            AdbStream stream = connectAndOpen(connection, peer, 9);
            byte[] chunk = "first chunk".getBytes(StandardCharsets.UTF_8);
            peer.send(AdbProtocol.A_WRTE, 9, 1, chunk);
            assertSilent(peer, 400);

            byte[] buffer = new byte[64];
            assertEquals(chunk.length, stream.read(buffer, 0, buffer.length));
            AdbProtocol.Message ready = peer.read();
            assertEquals(AdbProtocol.A_OKAY, ready.command);
            assertEquals(1, ready.arg0);
            assertEquals(9, ready.arg1);

            // The next payload is acknowledged the same way; a partial read acknowledges it too,
            // because the payload has left the queue for the stream's buffer.
            peer.send(AdbProtocol.A_WRTE, 9, 1, "second".getBytes(StandardCharsets.UTF_8));
            assertSilent(peer, 200);
            assertEquals(3, stream.read(buffer, 0, 3));
            assertEquals(AdbProtocol.A_OKAY, peer.read().command);
            assertEquals(3, stream.read(buffer, 0, 64));
            assertSilent(peer, 200);
            connection.close();
        }
    }

    /** A peer that ignores flow control loses that stream instead of filling the app's memory. */
    @Test
    public void aPeerThatIgnoresFlowControlLosesTheStream() throws Exception {
        try (Peer peer = new Peer()) {
            AdbConnection connection = AdbConnection.create("127.0.0.1", peer.port(), keyPair(), 30);
            AdbStream stream = connectAndOpen(connection, peer, 9);
            byte[] chunk = new byte[1024];
            for (int i = 0; i <= AdbStream.MAX_UNREAD_PAYLOADS; i++) {
                peer.send(AdbProtocol.A_WRTE, 9, 1, chunk);
            }
            AdbProtocol.Message close = peer.read();
            assertEquals(AdbProtocol.A_CLSE, close.command);
            assertEquals(1, close.arg0);
            assertEquals(9, close.arg1);
            try {
                stream.read(new byte[16], 0, 16);
                fail("The reader must learn why the stream ended");
            } catch (IOException e) {
                assertTrue(e.getMessage(), e.getMessage().contains("unacknowledged"));
            }
            assertTrue(stream.getFailure().contains("unacknowledged"));
            connection.close();
        }
    }

    @Test
    public void openDestinationsWithNonAsciiTextAreEncodedWhole() throws Exception {
        String destination = "shell,v2,raw:cat '/sdcard/Café résumé 写真.txt'";
        byte[] message = AdbProtocol.generateOpen(7, destination);
        byte[] expected = (destination + "\0").getBytes(StandardCharsets.UTF_8);
        AdbProtocol.Message parsed = AdbProtocol.Message.parse(new java.io.ByteArrayInputStream(message), AdbProtocol.A_VERSION_MIN, 1024 * 1024);
        assertEquals(AdbProtocol.A_OPEN, parsed.command);
        assertEquals(7, parsed.arg0);
        org.junit.Assert.assertArrayEquals(expected, parsed.payload);
    }
}
