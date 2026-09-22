#!/usr/bin/env python3
"""Build orphan-entry.zip: a valid ZIP whose first local entry is not
referenced by the central directory.

The 'mimetype' local entry occupies bytes 0..56 (30 header + 8 name + 19 data)
and no central directory record points at it; the single record points at
offset 57. This is the same shape as the Krita .kra archives referenced in the
issue, reproduced synthetically so no third-party bytes are needed.
"""
import struct
import zlib


def local_header(name, method, data, raw_len, crc):
    return struct.pack('<IHHHHHIIIHH', 0x04034b50, 20, 0, method, 0, 0,
                       crc, len(data), raw_len, len(name), 0) + name + data


def central(name, method, comp_size, raw_len, crc, offset):
    return struct.pack('<IHHHHHHIIIHHHHHII', 0x02014b50, 20, 20, 0, method,
                       0, 0, crc, comp_size, raw_len, len(name),
                       0, 0, 0, 0, 0, offset) + name


out = bytearray()

# The orphan: stored, 57 bytes total, never referenced below.
mt = b'application/x-krita'
out += local_header(b'mimetype', 0, mt, len(mt), zlib.crc32(mt) & 0xffffffff)
assert len(out) == 57

# The only entry the central directory knows about. Compressed at level 1 on
# purpose, so an optimizer has something to gain.
payload = (b'columbo synthetic reproducer: unreferenced local entry at offset 0.\n'
           b'The central directory below points at offset 57, not 0.\n') * 40
c = zlib.compressobj(1, zlib.DEFLATED, -15)
data = c.compress(payload) + c.flush()
crc = zlib.crc32(payload) & 0xffffffff
entry_off = len(out)
out += local_header(b'content.txt', 8, data, len(payload), crc)

cd_off = len(out)
cd = central(b'content.txt', 8, len(data), len(payload), crc, entry_off)
out += cd
out += struct.pack('<IHHHHIIH', 0x06054b50, 0, 0, 1, 1, len(cd), cd_off, 0)

with open('orphan-entry.zip', 'wb') as f:
    f.write(bytes(out))
print(f'orphan-entry.zip: {len(out)} bytes, orphan at 0..56, CD entry at {entry_off}')
