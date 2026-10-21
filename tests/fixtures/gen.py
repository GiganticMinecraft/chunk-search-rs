"""Generate the minimal Anvil worlds used by tests/cli.rs (Python stdlib only).

Usage: python3 tests/fixtures/gen.py tests/fixtures/worlds

The output is deterministic (timestamps are fixed to 0), so re-running this
script must not change the committed files unless the script itself changed.
"""
import gzip
import os
import shutil
import struct
import sys
import zlib

END, BYTE, SHORT, INT, LONG, FLOAT, DOUBLE, BARR, STRING, LIST, COMPOUND, IARR, LARR = range(13)

GZIP, ZLIB, NONE = 1, 2, 3
EXTERNAL = 128


class Int(int):
    pass


class Byte(int):
    pass


class List:
    def __init__(self, tagtype, items):
        self.tagtype, self.items = tagtype, items


class IntArray:
    def __init__(self, items):
        self.items = items


def _str(s):
    b = s.encode('utf-8')
    return struct.pack('>H', len(b)) + b


def tagtype(v):
    if isinstance(v, Byte):
        return BYTE
    if isinstance(v, (Int, int)):
        return INT
    if isinstance(v, str):
        return STRING
    if isinstance(v, List):
        return LIST
    if isinstance(v, IntArray):
        return IARR
    if isinstance(v, dict):
        return COMPOUND
    raise TypeError(v)


def payload(v):
    t = tagtype(v)
    if t == BYTE:
        return struct.pack('>b', v)
    if t == INT:
        return struct.pack('>i', v)
    if t == STRING:
        return _str(v)
    if t == LIST:
        return struct.pack('>bi', v.tagtype if v.items else END, len(v.items)) + b''.join(payload(i) for i in v.items)
    if t == IARR:
        return struct.pack('>i', len(v.items)) + b''.join(struct.pack('>i', i) for i in v.items)
    if t == COMPOUND:
        return b''.join(struct.pack('>b', tagtype(x)) + _str(k) + payload(x) for k, x in v.items()) + b'\x00'


def root(d):
    return b'\x0a' + _str('') + payload(d)


def chest(x, y, z):
    return {'id': 'minecraft:chest', 'x': Int(x), 'y': Int(y), 'z': Int(z), 'keepPacked': Byte(0), 'Items': List(COMPOUND, [])}


def chunk_118(cx, cz, with_be=True):
    """1.18.2 (DataVersion 2975): no Level, fields at the root, block_entities."""
    return {'DataVersion': Int(2975), 'xPos': Int(cx), 'zPos': Int(cz), 'yPos': Int(-4), 'Status': 'full',
            'sections': List(COMPOUND, []),
            'block_entities': List(COMPOUND, [chest(cx * 16 + 1, 64, cz * 16 + 1)] if with_be else [])}


def chunk_116(cx, cz, with_te=True, data_version=2586, with_entities_tag=True):
    """1.16 and older: everything under Level. 1.17 drops Level.Entities."""
    level = {'xPos': Int(cx), 'zPos': Int(cz), 'Status': 'full', 'Sections': List(COMPOUND, [])}
    if with_entities_tag:
        level['Entities'] = List(COMPOUND, [])
    level['TileEntities'] = List(COMPOUND, [chest(cx * 16 + 1, 64, cz * 16 + 1)] if with_te else [])
    c = {}
    if data_version is not None:
        c['DataVersion'] = Int(data_version)
    c['Level'] = level
    return c


def entity_chunk(cx, cz, entities):
    """1.17+ entities/r.*.mca chunk."""
    return {'DataVersion': Int(2975), 'Position': IntArray([cx, cz]),
            'Entities': List(COMPOUND, [{'id': e} for e in entities])}


def compress(scheme, data):
    if scheme == GZIP:
        return gzip.compress(data, mtime=0)
    if scheme == ZLIB:
        return zlib.compress(data)
    return data


def write_region(path, chunks, external=(), schemes=None, raw=None):
    """chunks: {(lx, lz): nbt_dict}.

    external: local positions written as c.<x>.<z>.mcc (scheme | 128).
    schemes: {(lx, lz): compression scheme} (default zlib).
    raw: {(lx, lz): (scheme byte, payload bytes)} written as-is (for broken chunks).
    """
    schemes = schemes or {}
    raw = raw or {}
    loc = bytearray(4096)
    ts = bytearray(4096)
    body = bytearray()
    sector = 2
    rx, rz = map(int, os.path.basename(path).split('.')[1:3])
    entries = [(pos, None) for pos in chunks] + [(pos, r) for pos, r in raw.items()]
    for (lx, lz), r in sorted(entries):
        if r is not None:
            scheme, data = r
            rec = struct.pack('>IB', len(data) + 1, scheme) + data
        else:
            scheme = schemes.get((lx, lz), ZLIB)
            data = compress(scheme, root(chunks[(lx, lz)]))
            if (lx, lz) in external:
                with open(os.path.join(os.path.dirname(path), f'c.{rx * 32 + lx}.{rz * 32 + lz}.mcc'), 'wb') as f:
                    f.write(data)
                rec = struct.pack('>IB', 1, scheme | EXTERNAL)
            else:
                rec = struct.pack('>IB', len(data) + 1, scheme) + data
        n = (len(rec) + 4095) // 4096
        rec += b'\x00' * (n * 4096 - len(rec))
        i = 4 * ((lx & 31) + (lz & 31) * 32)
        loc[i:i + 4] = struct.pack('>I', (sector << 8) | n)
        body += rec
        sector += n
    with open(path, 'wb') as f:
        f.write(loc + ts + body)


def world(base, name, chunks, region=(0, 0), subdir='region', **kw):
    d = os.path.join(base, name, subdir)
    os.makedirs(d, exist_ok=True)
    write_region(os.path.join(d, f'r.{region[0]}.{region[1]}.mca'), chunks, **kw)
    return d


def main(base):
    shutil.rmtree(base, ignore_errors=True)
    # 1.12 (no DataVersion) / 1.16
    world(base, 'w112', {(0, 0): chunk_116(0, 0, data_version=None)})
    world(base, 'w116', {(0, 0): chunk_116(0, 0)})
    world(base, 'w116_empty', {(0, 0): chunk_116(0, 0, with_te=False)})
    # 1.17: Level without Entities (moved to entities/)
    world(base, 'w117', {(0, 0): chunk_116(0, 0, data_version=2730, with_entities_tag=False),
                         (1, 0): chunk_116(1, 0, with_te=False, data_version=2730, with_entities_tag=False)})
    # 1.18.2
    world(base, 'w1182', {(0, 0): chunk_118(0, 0)})
    # region upgraded partially: old-format chunk next to a 1.18 chunk
    world(base, 'w1182_mixed', {(0, 0): chunk_116(0, 0), (1, 0): chunk_118(1, 0)})
    # oversized chunk stored in c.0.0.mcc
    world(base, 'w1182_external', {(0, 0): chunk_118(0, 0)}, external={(0, 0)})
    # gzip and uncompressed chunks
    world(base, 'w1182_compression', {(0, 0): chunk_118(0, 0), (1, 0): chunk_118(1, 0)},
          schemes={(0, 0): GZIP, (1, 0): NONE})
    # entity-only chunk in entities/ alongside region/
    world(base, 'w1182_entities_only', {(0, 0): chunk_118(0, 0, with_be=False)})
    world(base, 'w1182_entities_only', {(0, 0): entity_chunk(0, 0, ['minecraft:armor_stand'])}, subdir='entities')
    # negative region coordinates, external chunk, entities overlapping region results,
    # and an empty entity chunk
    world(base, 'w1182_negative', {(31, 31): chunk_118(-1, -1), (30, 31): chunk_118(-2, -1)},
          region=(-1, -1), external={(30, 31)})
    world(base, 'w1182_negative', {(31, 31): entity_chunk(-1, -1, ['minecraft:item_frame']),
                                   (30, 29): entity_chunk(-2, -3, ['minecraft:armor_stand']),
                                   (29, 29): entity_chunk(-3, -3, [])},
          region=(-1, -1), subdir='entities')
    # broken chunks are skipped with a warning; good chunk (0,0) is still reported
    world(base, 'w1182_corrupt', {(0, 0): chunk_118(0, 0)},
          raw={(1, 0): (ZLIB, b'this is not zlib'),
               (2, 0): (ZLIB, zlib.compress(root({'DataVersion': Int(2975)})))})  # no xPos
    # c.*.mcc referenced but missing -> warning
    d = world(base, 'w1182_missing_external', {(0, 0): chunk_118(0, 0), (1, 0): chunk_118(1, 0)}, external={(1, 0)})
    os.remove(os.path.join(d, 'c.1.0.mcc'))
    # LZ4 (scheme 4, 1.20.5+) is not supported -> non-zero exit
    world(base, 'w1182_unknown_compression', {(0, 0): chunk_118(0, 0)}, raw={(1, 0): (4, b'\x00' * 16)})
    # Bukkit nether world folder: world_nether/DIM-1/region
    world(base, 'w_nether', {(0, 0): chunk_118(0, 0)}, subdir=os.path.join('DIM-1', 'region'))
    print('ok')


if __name__ == '__main__':
    main(sys.argv[1])
