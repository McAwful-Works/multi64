"""Find the RAM addresses a ROM computes, and flag any that bracket the agent.

    ram-bounds.py <rom.z64> <agent_start> <agent_end> [--band 0x200000] [--code LO:HI ...]

A candidate region for the agent is usually cleared by two checks: it measured untouched
during play, and nothing in the ROM points into it. Both can pass while the region is
still doomed.

Castlevania: Legacy of Darkness is the worked example. Its main heap ends at 0x80400000
normally and 0x80634000 in the game's high quality mode, chosen by two instructions at
0x80000680. The agent sat at 0x80480000, inside the second. A pointer scan could never
have found it: a heap is defined by its BOUNDS, and neither bound is an address in the
region -- they are the addresses on either side of it.

So this reports every RAM address the code builds with lui+addiu/ori anywhere near the
agent, and flags the pairs that bracket it. A bracketing pair is not proof of anything; it
is the thing to go and disassemble.

A lui is only a lui if the word is really an instruction, and float tables read as `lui`
beautifully: any float whose top byte is 0x3C-0x3F -- most of a normalized table between
about 0.008 and 2 -- carries the lui opcode in bits 31-26 and its mantissa in the immediate.
Two rules cut nearly all of that. An addiu or ori only counts when its source register is
the one the lui loaded, which a mantissa matches by chance one time in 32. And a lui
adjacent to another lui is dropped, because a ramp of floats is a run of them while real
code is not. On the four games here that leaves Legacy of Darkness's two heap bounds and
nothing else: Castlevania 64, Paper Mario and Kirby 64 all come back clean, and each of
them had a float-table hit before the rules went in.

Give --code for each ROM range that really holds instructions. Without it the whole file is
scanned, and a 16 MiB ROM of textures and audio gives the rules far more chances to be
unlucky than they deserve.

Exit status is 0 when nothing brackets the agent and 1 when something does, so it composes
into a shell chain. A bad argument or an unreadable ROM is 2, not 1: a scan that never ran
must not read as a scan that found something.
"""
import struct
import sys

LUI, ADDIU, ORI = 0x0F, 0x09, 0x0D


def addresses(rom, spans):
    """Every (offset, address) a lui+addiu or lui+ori pair would build, within spans."""
    out = []
    for lo, hi in spans:
        out.extend(_in_span(rom, lo, hi))
    return out


def _in_span(rom, lo, hi):
    chunk = rom[lo:hi]
    words = struct.unpack(f">{len(chunk) // 4}I", chunk[: len(chunk) // 4 * 4])
    out = []
    for i in range(len(words) - 1):
        a, b = words[i], words[i + 1]
        if a >> 26 != LUI:
            continue
        hi2 = (a & 0xFFFF) << 16
        rt = (a >> 16) & 0x1F              # the register the lui loaded
        op, rs = b >> 26, (b >> 21) & 0x1F
        if op == ADDIU and rs == rt:
            addr = (hi2 + struct.unpack(">h", struct.pack(">H", b & 0xFFFF))[0]) & 0xFFFFFFFF
        elif op == ORI and rs == rt:
            addr = hi2 | (b & 0xFFFF)
        elif op == LUI or (i and words[i - 1] >> 26 == LUI):
            # A run of "lui"s is a float table, not code. Any float whose top byte is
            # 0x3C-0x3F -- which is most of a normalized table between about 0.008 and 2 --
            # has the lui opcode in bits 31-26, and its mantissa lands in the immediate.
            # Real code does not put three of them in a row; a ramp of floats always does.
            continue
        else:
            addr = hi2                     # a bare lui is a bound often enough to keep
        out.append((lo + i * 4, addr))
    return out


def main():
    try:
        rom_path = sys.argv[1]
        lo = int(sys.argv[2], 0)
        hi = int(sys.argv[3], 0)
        band = int(sys.argv[sys.argv.index("--band") + 1], 0) if "--band" in sys.argv else 0x200000
        rom = open(rom_path, "rb").read()
    except (IndexError, ValueError):
        sys.stderr.write(__doc__.split("\n\n")[1].strip() + "\n")
        return 2
    except OSError as e:
        sys.stderr.write(f"{e}\n")
        return 2

    spans = []
    for i, arg in enumerate(sys.argv):
        if arg == "--code":
            a, _, b = sys.argv[i + 1].partition(":")
            spans.append((int(a, 0), int(b, 0)))
    if not spans:
        spans = [(0, len(rom))]
        print("warning: no --code given, scanning the whole ROM; expect data read as code")

    found = {}
    for off, addr in addresses(rom, spans):
        if lo - band <= addr <= hi + band and addr >= 0x80000000:
            found.setdefault(addr, []).append(off)

    print(f"{rom_path}")
    print(f"agent {lo:#010x}-{hi:#010x}, looking {band:#x} either side\n")
    if not found:
        print("no RAM address built anywhere near the agent")
        return 0

    below = sorted(a for a in found if a < lo)
    inside = sorted(a for a in found if lo <= a < hi)
    above = sorted(a for a in found if a >= hi)

    for label, group in (("below", below), ("INSIDE THE AGENT", inside), ("above", above)):
        if group:
            print(f"{label}:")
            for a in group:
                offs = found[a]
                where = ", ".join(f"{o:#x}" for o in offs[:4])
                more = f" (+{len(offs) - 4} more)" if len(offs) > 4 else ""
                print(f"  {a:#010x}  built {len(offs)}x at rom {where}{more}")

    if below and above:
        print(f"\nBRACKETED: {below[-1]:#010x} below and {above[0]:#010x} above the agent.")
        print("A pair like that is how a heap's extent is written. Disassemble both before")
        print("trusting the region -- and check every mode the player can choose.")
        return 1
    print("\nNothing brackets the agent.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
