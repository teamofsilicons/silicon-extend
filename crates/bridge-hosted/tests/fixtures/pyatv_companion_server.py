"""A Companion-protocol server built from pyatv's own server-side pairing code.

Used by `appletv::companion::tests::interop_with_pyatv_server` to check the Rust client against an
independent implementation (pyatv's CompanionServerAuth, OPACK and ChaCha20 layers) rather than
against the Rust mock, which shares code with the client.

Run: python pyatv_companion_server.py   (needs `pip install pyatv`)
Prints `PORT <n>` once listening, then one JSON line per decrypted request. The pairing PIN is
pyatv's fixed server PIN (1111).
"""

import asyncio
import json

from pyatv.protocols.companion.connection import FrameType
from pyatv.protocols.companion.server_auth import CompanionServerAuth
from pyatv.support import chacha20, opack

AUTH_FRAMES = {FrameType.PS_Start, FrameType.PS_Next, FrameType.PV_Start, FrameType.PV_Next}


def jsonable(v):
    if isinstance(v, bytes):
        return v.hex()
    if isinstance(v, dict):
        return {str(k): jsonable(x) for k, x in v.items()}
    if isinstance(v, list):
        return [jsonable(x) for x in v]
    return v


class Server(CompanionServerAuth, asyncio.Protocol):
    def __init__(self):
        CompanionServerAuth.__init__(self, "Fake Apple TV")
        self.buf = b""
        self.chacha = None
        self.transport = None

    def connection_made(self, transport):
        self.transport = transport

    def enable_encryption(self, output_key, input_key):
        self.chacha = chacha20.Chacha20Cipher(output_key, input_key, nonce_length=12)

    def send_frame(self, frame_type, payload):
        length = len(payload) + (16 if self.chacha and payload else 0)
        header = bytes([frame_type.value]) + length.to_bytes(3, byteorder="big")
        if self.chacha and payload:
            payload = self.chacha.encrypt(payload, aad=header)
        self.transport.write(header + payload)

    def send_to_client(self, frame_type, data):
        self.send_frame(frame_type, opack.pack(data))

    def data_received(self, data):
        self.buf += data
        while len(self.buf) >= 4:
            length = int.from_bytes(self.buf[1:4], byteorder="big")
            if len(self.buf) < 4 + length:
                return
            header, payload = self.buf[:4], self.buf[4 : 4 + length]
            self.buf = self.buf[4 + length :]
            if self.chacha and payload:
                payload = self.chacha.decrypt(payload, aad=header)
            frame_type = FrameType(header[0])
            message, _ = opack.unpack(payload)
            if frame_type in AUTH_FRAMES:
                self.handle_auth_frame(frame_type, message)
                continue
            print(json.dumps({"frame": frame_type.name, "message": jsonable(message)}), flush=True)
            if message.get("_t") == 2:
                content = {}
                if message.get("_i") == "_sessionStart":
                    content = {"_sid": 77}
                elif message.get("_i") == "FetchLaunchableApplicationsEvent":
                    content = {"com.apple.TVSettings": "Settings", "com.netflix.Netflix": "Netflix"}
                self.send_to_client(FrameType.E_OPACK, {"_c": content, "_t": 3, "_x": message["_x"]})


async def main():
    loop = asyncio.get_running_loop()
    server = await loop.create_server(Server, "127.0.0.1", 0)
    print(f"PORT {server.sockets[0].getsockname()[1]}", flush=True)
    await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
