#!/usr/bin/env python3
"""Unauthenticated RP handshake over USB; never reads or writes pair records."""
import argparse
import datetime
import json
import plistlib
import socket
import struct


def exact(sock, count):
    chunks = bytearray()
    while len(chunks) < count:
        part = sock.recv(count - len(chunks))
        if not part:
            raise ConnectionError("peer closed before complete response")
        chunks.extend(part)
    return bytes(chunks)


def mux_request(sock, body):
    payload = plistlib.dumps({"MessageType": body.pop("MessageType"),
                             "ClientVersionString": "ipu-selfhost-probe", "ProgName": "ipu-selfhost-probe", **body})
    sock.sendall(struct.pack("<IIII", len(payload) + 16, 1, 8, 1) + payload)
    length, version, kind, tag = struct.unpack("<IIII", exact(sock, 16))
    if not 16 <= length <= 1048576 or version != 1 or kind != 8:
        raise ValueError("invalid usbmux response")
    return plistlib.loads(exact(sock, length - 16))


def run(udid=None, host=None):
    sock = None
    try:
        if host:
            sock = socket.create_connection((host, 49152), timeout=5)
        else:
            sock = socket.socket(socket.AF_UNIX)
            sock.settimeout(5)
            sock.connect("/var/run/usbmuxd")
            devices = mux_request(sock, {"MessageType": "ListDevices"}).get("DeviceList", [])
            device = next(d for d in devices if d.get("Properties", {}).get("SerialNumber") == udid
                          and d.get("Properties", {}).get("ConnectionType") == "USB")
            reply = mux_request(sock, {"MessageType": "Connect", "DeviceID": device["DeviceID"],
                                       "PortNumber": socket.htons(49152)})
            if reply.get("Number") != 0:
                return {"outcome": "usbmux_connect_failed", "code": reply.get("Number")}
        request = {"originatedBy": "host", "sequenceNumber": 0,
                   "message": {"plain": {"_0": {"request": {"_0": {"handshake": {"_0": {
                       "hostOptions": {"attemptPairVerify": True}, "wireProtocolVersion": 19}}}}}}}}
        body = json.dumps(request, separators=(",", ":")).encode()
        sock.sendall(b"RPPairing" + struct.pack(">H", len(body)) + body)
        header = exact(sock, 11)
        if header[:9] != b"RPPairing":
            return {"outcome": "handshake_unrecognized"}
        length = struct.unpack(">H", header[9:])[0]
        envelope = json.loads(exact(sock, length))
        if envelope.get("originatedBy") != "device":
            return {"outcome": "handshake_unrecognized"}
        details = envelope["message"]["plain"]["_0"]["response"]["_1"]["handshake"]["_0"]
        return {"outcome": "remote_pairing_handshake", "wire_protocol": details.get("wireProtocolVersion")}
    except StopIteration:
        return {"outcome": "usb_device_not_found"}
    except (OSError, ConnectionError, ValueError, KeyError) as error:
        return {"outcome": "handshake_failed", "error_type": type(error).__name__,
                "errno": getattr(error, "errno", None)}
    finally:
        if sock is not None:
            sock.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    transport = parser.add_mutually_exclusive_group(required=True)
    transport.add_argument("--udid")
    transport.add_argument("--host")
    args = parser.parse_args()
    report = {"schema": 1, "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "executor": "Mac network control" if args.host else "Mac USB control", "scope": "unauthenticated RemotePairing handshake only",
              "endpoint": {"host": args.host or "USB", "port": 49152},
              "result": run(args.udid, args.host)}
    print(json.dumps(report, indent=2, sort_keys=True))
