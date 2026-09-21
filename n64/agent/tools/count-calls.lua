-- Count how often each candidate function actually runs, per frame.
-- BizHawk: EmuHawk.exe --lua=count-calls.lua <rom>. Writes count-calls.txt next to itself.
-- Lua 5.1: BizHawk's NLua core is 5.1, so no `//` and nothing else 5.3 added.
--
-- Picking a per-frame hook site by reading code is how this repo got a site that fires
-- twice in three thousand frames. A loop is not automatically the frame loop, and a
-- function named like an update is not automatically called once a frame. This measures
-- it: execution hooks on each candidate, counted against emu.framecount().
--
-- Read the result as a rate, not a total. About 1.0 per frame is a frame function. Much
-- less is a state machine or an event. Much more is an inner loop, which still works for
-- an agent but costs more than it needs to.

local CANDIDATES = {
    -- call sites, not function entries: a function may be per-frame while any one of its
    -- callers is not, and a hook retargets one caller.
    { name = "gfxTask site 1",  addr = 0x80005D84 },
    { name = "gfxTask site 2",  addr = 0x80005DCC },
    { name = "gfxEnd site 1",   addr = 0x800059C0 },
    { name = "gfxEnd site 2",   addr = 0x80005A3C },
    { name = "gfxEnd site 3",   addr = 0x80006FF0 },
    { name = "omUpdateAll ovl2", addr = 0x800F6A90 },
}

local DOMAIN = "System Bus"
local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "count-calls.txt"
local START_FRAME = 120

local counts = {}
local attached = {}

for _, c in ipairs(CANDIDATES) do
    counts[c.name] = 0
    local ok = pcall(function()
        event.onmemoryexecute(function()
            counts[c.name] = counts[c.name] + 1
        end, c.addr, "count_" .. c.name, DOMAIN)
    end)
    attached[c.name] = ok
end

local base_frame = emu.framecount()

local function report()
    local frames = emu.framecount() - base_frame
    if frames < 1 then frames = 1 end
    local lines = { ("frame=%d  counted over %d frames"):format(emu.framecount(), frames) }
    for _, c in ipairs(CANDIDATES) do
        if attached[c.name] then
            local n = counts[c.name]
            local per = n / frames
            local verdict = "rare (state change or event)"
            if per >= 0.9 and per <= 1.1 then
                verdict = "ONCE PER FRAME"
            elseif per > 1.1 then
                verdict = "more than once a frame"
            elseif per > 0.1 then
                verdict = "often, but not every frame"
            end
            lines[#lines + 1] = ("  %-20s 0x%08X  %8d calls  %6.2f/frame  %s")
                :format(c.name, c.addr, n, per, verdict)
        else
            lines[#lines + 1] = ("  %-20s 0x%08X  execution hooks unavailable on this core")
                :format(c.name, c.addr)
        end
    end
    local f = io.open(out, "w")
    if f then f:write(table.concat(lines, "\n"), "\n") f:close() end
    return lines
end

while emu.framecount() < START_FRAME do
    gui.text(10, 50, "count-calls: waiting for boot (" .. emu.framecount() .. ")")
    emu.frameadvance()
end

-- Start counting from a settled game, not from boot.
for _, c in ipairs(CANDIDATES) do counts[c.name] = 0 end
base_frame = emu.framecount()

local lines = report()
while true do
    if emu.framecount() % 30 == 0 then lines = report() end
    for i, line in ipairs(lines) do
        gui.text(10, 50 + (i - 1) * 16, line)
    end
    emu.frameadvance()
end
