-- Map which RDRAM a running game uses, all 8 MB, to find room for the agent.
-- BizHawk: EmuHawk.exe --lua=ram-usage.lua <rom>. Writes ram-usage.txt next to itself.
--
-- Every SAMPLE_EVERY frames each 4 KB page of RDRAM is hashed. A page is USED if it
-- was non-zero on the first sample or its hash has ever changed. Reported: runs of
-- pages that were zero and never changed, largest first, split into the base 4 MB and
-- the Expansion Pak.
--
-- This is evidence, not proof. Heaps fill during play, buffers are touched only by
-- rare events, and an emulator running the RSP at a high level never shows the RSP's
-- own writes. Play through variety, then check every candidate against what the game
-- is known to keep there. See docs/integration/placing-the-agent.md.
local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "ram-usage.txt"

local DOMAIN = "RDRAM"
local PAGE = 0x1000
local SAMPLE_EVERY = 60
local NEED = 21 * 1024 -- agent code + data + bss, rounded up

local function u32(addr) return memory.read_u32_be(addr, DOMAIN) end

local top = math.min(memory.getmemorydomainsize(DOMAIN), 0x800000)
local npages = top // PAGE
local used, last_hash = {}, {}
local samples, changes = 0, 0

local function page_is_zero(addr)
    local bytes = memory.read_bytes_as_array(addr, PAGE, DOMAIN)
    for i = 1, #bytes do if bytes[i] ~= 0 then return false end end
    return true
end

local function sample()
    samples = samples + 1
    for p = 0, npages - 1 do
        local addr = p * PAGE
        local h = memory.hash_region(addr, PAGE, DOMAIN)
        if samples == 1 then
            if not page_is_zero(addr) then used[p] = true end
        elseif last_hash[p] ~= h then
            if not used[p] then changes = changes + 1 end
            used[p] = true
        end
        last_hash[p] = h
    end
end

local function runs_in(lo_page, hi_page)
    local runs, start = {}, nil
    for p = lo_page, hi_page do
        local free = p < hi_page and not used[p]
        if free and not start then start = p
        elseif not free and start then runs[#runs + 1] = { start, p - start }; start = nil end
    end
    table.sort(runs, function(a, b) return a[2] > b[2] end)
    return runs
end

local function report()
    local lines = {}
    local used_lo, used_hi = 0, 0
    for p = 0, npages - 1 do
        if used[p] then if p < 0x400 then used_lo = used_lo + 1 else used_hi = used_hi + 1 end end
    end
    -- The RDRAM domain size is not the console's memory size in every BizHawk version;
    -- osMemSize (u32 at 0x318) is what the game itself uses.
    lines[#lines + 1] = ("frame=%d samples=%d osMemSize=0x%X domain=0x%X used_pages: base4MB=%d/1024 expansion=%d/%d newly_touched=%d"):format(
        emu.framecount(), samples, u32(0x318), top, used_lo, used_hi, math.max(npages - 0x400, 0), changes)
    for _, part in ipairs({ { "base 4 MB", 0, math.min(npages, 0x400) }, { "Expansion Pak", 0x400, npages } }) do
        if part[3] > part[2] then
            lines[#lines + 1] = ("never-touched zero runs in %s (VRAM range, KB):"):format(part[1])
            local runs = runs_in(part[2], part[3])
            for i = 1, math.min(#runs, 10) do
                local s, n = runs[i][1], runs[i][2]
                lines[#lines + 1] = ("  0x%08X-0x%08X  %5d KB%s"):format(0x80000000 + s * PAGE,
                    0x80000000 + (s + n) * PAGE, n * PAGE // 1024, (n * PAGE >= NEED) and "  fits agent" or "")
            end
        end
    end
    lines[#lines + 1] = "used map (one char per 64 KB, '#' used, '.' untouched):"
    local map = {}
    for chunk = 0, npages - 1, 16 do
        local any = false
        for p = chunk, math.min(chunk + 15, npages - 1) do if used[p] then any = true end end
        map[#map + 1] = any and "#" or "."
        if chunk + 16 == 0x400 then map[#map + 1] = "|" end
    end
    lines[#lines + 1] = "  0x80000000 " .. table.concat(map) .. " 0x" .. string.format("%08X", 0x80000000 + top)
    local f = io.open(out, "w")
    if f then f:write(table.concat(lines, "\n"), "\n"); f:close() end
    return lines[1]
end

-- Wait out boot, so the first sample is the game's steady state rather than IPL
-- leftovers; mods often copy code into RAM during boot.
while emu.framecount() < 300 do
    gui.text(10, 10, "ram-usage: waiting for boot (" .. emu.framecount() .. ")")
    emu.frameadvance()
end

local status = "sampling..."
while true do
    if emu.framecount() % SAMPLE_EVERY == 0 then
        sample()
        status = report()
    end
    gui.text(10, 10, "ram-usage: " .. status)
    emu.frameadvance()
end
