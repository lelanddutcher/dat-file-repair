"""Walk Canon 'repi' recovery-index headers in a .DAT and summarize track types.
Layout (little-endian, per anthwlock/untrunc src/canon.cpp):
  'repi' | u24 header_len | 0x83 | type[4] | u32 fixed_size | u32 n | [u32 size]*n (if fixed_size==0)
Samples follow back to back; padding between groups is found by scanning for the next 'repi'."""
import sys, struct, collections
path = sys.argv[1]
data = open(path, 'rb').read()
pos = data.find(b'repi')
stats = collections.OrderedDict(); order = []; gaps = collections.Counter(); groups = 0; last_end = None
while pos != -1 and pos + 20 <= len(data):
    hl = int.from_bytes(data[pos+4:pos+7], 'little'); flag = data[pos+7]
    typ = data[pos+8:pos+12].decode('latin1'); fixed, n = struct.unpack_from('<II', data, pos+12)
    if flag != 0x83 or hl != 20 + (0 if fixed else 4*n):
        print(f'bad header at {pos}: hl={hl} flag={flag:#x} typ={typ!r} fixed={fixed} n={n}'); break
    sizes = [fixed]*n if fixed else list(struct.unpack_from(f'<{n}I', data, pos+20))
    payload = sum(sizes); end = pos + hl + payload
    if last_end is not None: gaps[pos - last_end] += 1
    s = stats.setdefault(typ, [0, 0, 0, set()]); s[0] += 1; s[1] += n; s[2] += payload
    if fixed: s[3].add(fixed)
    if len(order) < 24: order.append(f'{typ}x{n}')
    groups += 1; last_end = end
    if end > len(data): print(f'truncated group at {pos} (needs {end-len(data)} more bytes)'); break
    pos = data.find(b'repi', end)
print(f'file={path} size={len(data)} groups={groups}')
for t, (g, n, b, fx) in stats.items():
    print(f'  {t!r}: groups={g} samples={n} bytes={b} fixed_sizes={sorted(fx)[:5]}')
print('  first groups:', ' '.join(order))
print('  gap sizes between groups (bytes:count):', dict(gaps.most_common(8)))
