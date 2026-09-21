-- Watch named RAM ranges byte for byte and report the first frame each one changes.
-- BizHawk: EmuHawk.exe --lua=watch-ranges.lua <rom>. Writes watch-ranges.txt next to itself.
-- Lua 5.1: BizHawk's NLua core is 5.1, so no `//` and nothing else 5.3 added.
--
-- ram-usage.lua answers "which 4 KB pages does this game use", which is the right question
-- when looking for somewhere to put the agent. It is the wrong question for a few hundred
-- bytes of padding inside a code segment: the page around them is busy whatever they do.
-- This watches the exact bytes, so a candidate is cleared or killed on its own evidence.
--
-- Edit RANGES. Addresses are the ones a disassembly shows (0x80...); the RDRAM domain is
-- indexed from 0, so the script subtracts the base itself.

local RANGES = {
    { name = "k64 main zero run A", first = 0x8003BD0C, last = 0x8003C14C },
    { name = "k64 main zero run B", first = 0x8003AA88, last = 0x8003ADBC },
    { name = "k64 main zero run C", first = 0x8003D11C, last = 0x8003D2DC },
}

local DOMAIN = "RDRAM"
local BASE = 0x80000000
local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "watch-ranges.txt"

-- Wait out boot: the segment holding these bytes has to be in RAM before there is
-- anything to compare against.
local START_FRAME = 300

local watched = {}

local function snapshot(r)
    local bytes = memory.read_bytes_as_array(r.addr, r.len, DOMAIN)
    local zero = true
    for i = 1, #bytes do
        if bytes[i] ~= 0 then zero = false break end
    end
    return bytes, zero
end

-- hash_region is native and cheap; the byte compare only runs once, when it disagrees.
local function changed(r)
    return memory.hash_region(r.addr, r.len, DOMAIN) ~= r.hash
end

local function first_difference(r)
    local now = memory.read_bytes_as_array(r.addr, r.len, DOMAIN)
    for i = 1, #now do
        if now[i] ~= r.bytes[i] then
            return i - 1, r.bytes[i], now[i]
        end
    end
    return nil
end

local function report()
    local lines = { ("frame=%d  watching %d ranges"):format(emu.framecount(), #watched) }
    for _, r in ipairs(watched) do
        if r.changed_at then
            lines[#lines + 1] = ("  %-24s 0x%08X+%-5d IN USE: changed at frame %d, first at +0x%X (0x%02X -> 0x%02X)")
                :format(r.name, r.first, r.len, r.changed_at, r.diff_off, r.was, r.now)
        else
            lines[#lines + 1] = ("  %-24s 0x%08X+%-5d untouched so far%s")
                :format(r.name, r.first, r.len, r.all_zero and " (and still all zero)" or "")
        end
    end
    local f = io.open(out, "w")
    if f then f:write(table.concat(lines, "\n"), "\n") f:close() end
    return lines
end

while emu.framecount() < START_FRAME do
    gui.text(10, 30, "watch-ranges: waiting for boot (" .. emu.framecount() .. ")")
    emu.frameadvance()
end

for _, r in ipairs(RANGES) do
    local w = { name = r.name, first = r.first, addr = r.first - BASE, len = r.last - r.first }
    w.bytes, w.all_zero = snapshot(w)
    w.hash = memory.hash_region(w.addr, w.len, DOMAIN)
    watched[#watched + 1] = w
end

local lines = report()
local dirty = false

while true do
    for _, r in ipairs(watched) do
        if not r.changed_at and changed(r) then
            local off, was, now = first_difference(r)
            if off then
                r.changed_at, r.diff_off, r.was, r.now = emu.framecount(), off, was, now
                dirty = true
            end
        end
    end
    if dirty or emu.framecount() % 30 == 0 then
        lines = report()
        dirty = false
    end
    for i, line in ipairs(lines) do
        gui.text(10, 30 + (i - 1) * 16, line)
    end
    emu.frameadvance()
end
