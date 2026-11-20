import struct, zlib, sys
def render(n, ss=4):
    N = n*ss; px = []
    R = N*0.22  # corner radius
    for y in range(n):
        row = bytearray()
        for x in range(n):
            acc_bg = acc_fg = 0
            for sy in range(ss):
                for sx in range(ss):
                    X = x*ss+sx+0.5; Y = y*ss+sy+0.5
                    # rounded square
                    dx = max(R - X, 0, X - (N - R)); dy = max(R - Y, 0, Y - (N - R))
                    if dx*dx+dy*dy <= R*R:
                        acc_bg += 1
                        cx, cy, r1 = N*0.5, N*0.5, N*0.30
                        ox, oy, r2 = N*0.62, N*0.40, N*0.26
                        if (X-cx)**2+(Y-cy)**2 <= r1*r1 and (X-ox)**2+(Y-oy)**2 > r2*r2:
                            acc_fg += 1
            t = ss*ss
            a = acc_bg/t; f = acc_fg/max(acc_bg,1)
            bg = (24, 24, 27); fg = (245, 245, 245)
            c = [round(bg[i]*(1-f)+fg[i]*f) for i in range(3)]
            row += bytes(c + [round(a*255)])
        px.append(bytes(row))
    raw = b''.join(b'\0'+r for r in px)
    def chunk(t, d): return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t+d) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', n, n, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')
sizes = [256, 64, 48, 32, 24, 16]
pngs = [render(s) for s in sizes]
out = struct.pack('<HHH', 0, 1, len(sizes)); off = 6 + 16*len(sizes)
for s, p in zip(sizes, pngs):
    out += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(p), off); off += len(p)
open(sys.argv[1], 'wb').write(out + b''.join(pngs))
open(sys.argv[2], 'wb').write(pngs[0])
