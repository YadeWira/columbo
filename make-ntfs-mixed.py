#!/usr/bin/env python3
"""Build ntfs-mixed.zip: two entries with the same payload, one LZMA
(method 14) and one Deflate (method 8), each carrying an NTFS timestamps
extra field (0x000a, 36 bytes) in its central directory record only.

It shows which entries a metadata-stripping pass reaches: the Deflate entry
is recompressed, the LZMA entry is not.

The LZMA bytes come from the standard lzma module, so they depend on the
liblzma version; CPython 3.13 reproduces the published file exactly.
"""
import lzma
import struct
import zlib


def local_header(name, method, data, raw_len, crc, flag):
    return struct.pack('<IHHHHHIIIHH', 0x04034b50, 20, flag, method, 0, 0,
                       crc, len(data), raw_len, len(name), 0) + name + data


def central(name, method, comp_size, raw_len, crc, offset, extra, flag):
    return struct.pack('<IHHHHHHIIIHHHHHII', 0x02014b50, 20, 20, flag, method,
                       0, 0, crc, comp_size, raw_len, len(name), len(extra),
                       0, 0, 0, 0, offset) + name + extra


# NTFS extra: 4 reserved bytes, then tag 0x0001 holding mtime, atime, ctime.
stamp = 132000000000000000
NTFS = struct.pack('<HH', 0x000a, 32) + struct.pack('<IHHQQQ', 0, 1, 24, stamp, stamp, stamp)

raw = (b'columbo synthetic reproducer: NTFS timestamps extra field '
       b'only in the central directory.\n') * 60
crc = zlib.crc32(raw) & 0xffffffff

# LZMA in ZIP: version, properties size, 5-byte properties, raw LZMA1 stream
# terminated by an end marker (general-purpose bit 1).
filters = [{'id': lzma.FILTER_LZMA1, 'dict_size': 1 << 16, 'lc': 3, 'lp': 0, 'pb': 2}]
lzma_data = (struct.pack('<BBH', 9, 20, 5) + bytes([(2 * 5 + 0) * 9 + 3])
             + struct.pack('<I', 1 << 16)
             + lzma.compress(raw, format=lzma.FORMAT_RAW, filters=filters))

c = zlib.compressobj(1, zlib.DEFLATED, -15)   # weak on purpose
deflate_data = c.compress(raw) + c.flush()

out = bytearray()
records = []
for name, method, data, flag in [(b'lzma.txt', 14, lzma_data, 0x0002),
                                 (b'deflate.txt', 8, deflate_data, 0)]:
    offset = len(out)
    out += local_header(name, method, data, len(raw), crc, flag)
    records.append(central(name, method, len(data), len(raw), crc, offset, NTFS, flag))

cd = b''.join(records)
cd_off = len(out)
out += cd
out += struct.pack('<IHHHHIIH', 0x06054b50, 0, 0, 2, 2, len(cd), cd_off, 0)

with open('ntfs-mixed.zip', 'wb') as f:
    f.write(bytes(out))
print(f'ntfs-mixed.zip: {len(out)} bytes')
