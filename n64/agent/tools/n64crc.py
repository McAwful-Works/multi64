"""Check or fix an N64 ROM header CRC after changing its code.

    python n64crc.py <rom.z64>          report the boot chip and whether the CRC verifies
    python n64crc.py --fix <rom.z64>    rewrite the CRC in place

The CRC covers ROM 0x1000-0x101000 (the first MiB after the header and IPL3), so any
change there -- a retargeted jal, a hook stub -- invalidates it, and a console whose
IPL3 checks it will not boot. The boot chip is identified from a CRC32 of IPL3
(0x40-0x1000).

Only CIC-6102 and CIC-6103 are implemented, because only those have been checked
against real ROMs here. A ROM with another boot chip, or an IPL3 that matches none
(for example one a patch has modified), is refused rather than guessed at.
"""
import struct
import sys
import zlib

M = 0xFFFFFFFF
IPL3_CRC32 = {0x90BB6CB5: "6102", 0x0B050EE0: "6103"}
SEEDS = {"6102": 0xF8CA4DDC, "6103": 0xA3886759}


def crc(rom, cic):
    t1 = t2 = t3 = t4 = t5 = t6 = SEEDS[cic]
    for i in range(0x1000, 0x101000, 4):
        d = struct.unpack_from(">I", rom, i)[0]
        if (t6 + d) & M < t6:
            t4 = (t4 + 1) & M
        t6 = (t6 + d) & M
        t3 ^= d
        s = d & 0x1F
        rot = ((d << s) | (d >> (32 - s))) & M if s else d
        t5 = (t5 + rot) & M
        t2 = t2 ^ rot if t2 > d else t2 ^ (t6 ^ d)
        t1 = (t1 + (t5 ^ d)) & M
    if cic == "6103":
        return ((t6 ^ t4) + t3) & M, ((t5 ^ t2) + t1) & M
    return (t6 ^ t4 ^ t3) & M, (t5 ^ t2 ^ t1) & M


def main():
    args = sys.argv[1:]
    fix = "--fix" in args
    paths = [a for a in args if a != "--fix"]
    if len(paths) != 1:
        sys.exit(__doc__)
    rom = bytearray(open(paths[0], "rb").read())
    if rom[:4] != b"\x80\x37\x12\x40":
        sys.exit("not a big-endian (.z64) ROM; convert it first")
    if len(rom) < 0x101000:
        sys.exit("ROM is shorter than the CRC region")

    ipl3 = zlib.crc32(bytes(rom[0x40:0x1000]))
    cic = IPL3_CRC32.get(ipl3)
    if cic is None:
        sys.exit(f"IPL3 CRC32 0x{ipl3:08X} matches no supported boot chip (6102, 6103); "
                 "restore the retail IPL3 or compute the CRC another way")

    want = crc(rom, cic)
    have = struct.unpack_from(">II", rom, 0x10)
    print(f"CIC-{cic}: header 0x{have[0]:08X} 0x{have[1]:08X}, computed 0x{want[0]:08X} 0x{want[1]:08X}"
          f" -> {'verifies' if tuple(want) == have else 'MISMATCH'}")
    if fix and tuple(want) != have:
        struct.pack_into(">II", rom, 0x10, *want)
        open(paths[0], "wb").write(rom)
        print("fixed")
    elif not fix and tuple(want) != have:
        sys.exit(1)


if __name__ == "__main__":
    main()
