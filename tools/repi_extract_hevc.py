"""Extract 'vide' samples from repi groups as Annex-B HEVC (4-byte length prefix -> start code)."""
import sys, struct
src, dst = sys.argv[1], sys.argv[2]
data = open(src, 'rb').read(); out = open(dst, 'wb'); pos = data.find(b'repi'); frames = 0; types = {}
while pos != -1:
    hl = int.from_bytes(data[pos+4:pos+7], 'little'); typ = data[pos+8:pos+12]
    fixed, n = struct.unpack_from('<II', data, pos+12)
    sizes = [fixed]*n if fixed else list(struct.unpack_from(f'<{n}I', data, pos+20))
    p = pos + hl
    for s in sizes:
        if p + s > len(data): break
        if typ == b'vide':
            q = p
            while q < p + s:
                L = int.from_bytes(data[q:q+4], 'big'); nt = (data[q+4] >> 1) & 0x3f
                if frames < 3: types.setdefault(frames, []).append(nt)
                out.write(b'\0\0\0\1' + data[q+4:q+4+L]); q += 4 + L
            frames += 1
        p += s
    pos = data.find(b'repi', p)
print('frames', frames, 'NAL types first 3 frames', types)
