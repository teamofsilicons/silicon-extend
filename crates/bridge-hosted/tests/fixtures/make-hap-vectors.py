"""Regenerates hap-vectors.json (OPACK, TLV8, HKDF, ChaCha20, SRP) from pyatv 0.18 and srptools.

Run: python make-hap-vectors.py  (needs `pip install pyatv`); writes vectors.json in the current
directory, which is hap-vectors.json minus the `_about` note.
"""
import binascii, hashlib, uuid, json
from pyatv.support import opack
from pyatv.auth.hap_tlv8 import write_tlv, read_tlv
from pyatv.auth.hap_srp import hkdf_expand
from pyatv.support.chacha20 import Chacha20Cipher8byteNonce, Chacha20Cipher
from srptools import SRPContext, SRPClientSession, SRPServerSession, constants
h=lambda b: binascii.hexlify(b).decode()
out={}
# OPACK
cases = {
 "small": {"_i":"_systemInfo","_t":2,"_x":12345},
 "types": [True, False, None, 0, 0x27, 0x28, 255, 256, 65535, 65536, 2**32, 1.5, "", "a"*32, "b"*33, "c"*300, b"", b"\x01"*32, b"\x02"*33, b"\x03"*300],
 "uuid": uuid.UUID("12345678-1234-5678-1234-567812345678"),
 "big_dict": {f"k{i}": i for i in range(16)},
 "big_list": list(range(16)),
 "refs": ["hello","hello",["hello"],{"hello":"hello"}, 1000, 1000],
 "nested": {"_c":{"_hBtS":1,"_hidC":6},"_i":"_hidC","_t":2,"_x":7},
}
for k,v in cases.items():
    out["opack_"+k]=h(opack.pack(v))
# TLV8
out["tlv_short"]=h(write_tlv({6:b"\x01",0:b"\x00"}))
out["tlv_long"]=h(write_tlv({3:bytes(range(256))*2, 6:b"\x03"}))
# HKDF
secret=bytes(range(32))
for salt,info in [("Pair-Setup-Encrypt-Salt","Pair-Setup-Encrypt-Info"),("Pair-Verify-Encrypt-Salt","Pair-Verify-Encrypt-Info"),("","ClientEncrypt-main"),("Control-Salt","Control-Write-Encryption-Key")]:
    out[f"hkdf_{salt}|{info}"]=h(hkdf_expand(salt,info,secret))
# chacha
key=bytes(range(32))
c=Chacha20Cipher8byteNonce(key,key)
out["chacha_ps_msg05"]=h(c.encrypt(b"hello pairing", nonce=b"PS-Msg05"))
c2=Chacha20Cipher(key,key,nonce_length=12)
out["chacha_counter12_0"]=h(c2.encrypt(b"frame0", aad=b"\x08\x00\x00\x16"))
out["chacha_counter12_1"]=h(c2.encrypt(b"frame1", aad=b"\x08\x00\x00\x16"))
c3=Chacha20Cipher(key,key)  # 8-byte counter nonce (HAP session)
out["chacha_counter8_0"]=h(c3.encrypt(b"GET / RTSP/1.0", aad=b"\x0e\x00"))
out["chacha_counter8_1"]=h(c3.encrypt(b"x", aad=b"\x01\x00"))
# SRP 3072 SHA512, fixed values
a_hex = "60975527035cf2ad1989806f0407210bc81edc04e2762a56afd529ddda2d4393"
b_hex = "e487cb59d31ac550471e81f00f6928e01dda08e974a004f49e61f5d105284d20"
salt_hex = "beb25379d1a8581eb5a727673a2441ee"
pin="1234"
ctx = SRPContext("Pair-Setup", pin, prime=constants.PRIME_3072, generator=constants.PRIME_3072_GEN, hash_func=hashlib.sha512)
x = ctx.get_common_password_hash(int(salt_hex,16))
v = ctx.get_common_password_verifier(x)
sctx = SRPContext("Pair-Setup", prime=constants.PRIME_3072, generator=constants.PRIME_3072_GEN, hash_func=hashlib.sha512)
server = SRPServerSession(sctx, '%x'%v, b_hex)
client = SRPClientSession(SRPContext("Pair-Setup", pin, prime=constants.PRIME_3072, generator=constants.PRIME_3072_GEN, hash_func=hashlib.sha512), a_hex)
client.process(server.public, salt_hex)
server.process(client.public, salt_hex)
out["srp_A"]=client.public; out["srp_B"]=server.public; out["srp_K"]=client.key; out["srp_M1"]=client.key_proof; out["srp_M2"]=client.key_proof_hash
assert server.key == client.key
assert server.verify_proof(client.key_proof)
# RFC 5054 1024 sha1
ctx1 = SRPContext("alice","password123")  # defaults 1024/sha1
s1=int("BEB25379D1A8581EB5A727673A2441EE",16)
x1=ctx1.get_common_password_hash(s1); v1=ctx1.get_common_password_verifier(x1)
a1=int("60975527035CF2AD1989806F0407210BC81EDC04E2762A56AFD529DDDA2D4393",16)
b1=int("E487CB59D31AC550471E81F00F6928E01DDA08E974A004F49E61F5D105284D20",16)
A1=ctx1.get_client_public(a1); B1=ctx1.get_server_public(v1,b1); u1=ctx1.get_common_secret(B1,A1)
S1=ctx1.get_client_premaster_secret(x1,B1,a1,u1)
out.update({"rfc_k":'%x'%ctx1._mult,"rfc_x":'%x'%x1,"rfc_v":'%x'%v1,"rfc_A":'%x'%A1,"rfc_B":'%x'%B1,"rfc_u":'%x'%u1,"rfc_S":'%x'%S1})
out={k:(v.decode() if isinstance(v,bytes) else v) for k,v in out.items()}
json.dump(out, open("vectors.json","w"), indent=1)
print(out["rfc_k"], out["rfc_x"], out["rfc_u"])
