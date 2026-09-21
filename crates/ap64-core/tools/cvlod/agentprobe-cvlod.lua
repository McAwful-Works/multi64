-- Watch the M64P agent spliced into a Castlevania: Legacy of Darkness seed.
-- BizHawk: EmuHawk.exe --lua=agentprobe-cvlod.lua <spliced rom>. Writes agentprobe-cvlod.txt
-- next to itself and draws the numbers on screen.
--
-- Set AGENT_VRAM, AGENT_ROM and AGENT_MIN_RAM to profiles/cvlod/layout.env. The offsets are
-- from nm on agent/cvlod/build/agent.elf (multi64 with PEEKROM); re-read them if the agent changes.
local AGENT_VRAM = 0x80480000
local AGENT_ROM = 0x1000000
local AGENT_MIN_RAM = 0x800000

local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "agentprobe-cvlod.txt"
local base = AGENT_VRAM - 0x80000000

local ADDR = {
    magic         = base + 0x2468, -- gAgentSegmentMagic, 0x4D363450 once loaded
    init_attempts = base + 0x246C, -- s_init_attempts (dormant for good at 16)
    ready         = base + 0x2470, -- s_ready
    frames        = base + 0x2474, -- s_frames_handled
    ticks         = base + 0x2478, -- s_ticks
    errors        = base + 0x6458, -- s_errors (mem_proto)
    requests      = base + 0x6464, -- s_requests (mem_proto)
}
local TEXT_LEN = 0x1000
local clobbered_at = nil

local function u32(addr) return memory.read_u32_be(addr, "RDRAM") end

local function snapshot()
    local osmem = u32(0x318)
    local magic = u32(ADDR.magic)
    if osmem < AGENT_MIN_RAM then
        return string.format("frame=%d osMemSize=0x%X below AGENT_MIN_RAM: agent must stay unloaded, magic=0x%08X",
            emu.framecount(), osmem, magic)
    end
    local text = "not loaded"
    if magic == 0x4D363450 then
        local same = memory.hash_region(base, TEXT_LEN, "RDRAM") == memory.hash_region(AGENT_ROM, TEXT_LEN, "ROM")
        if not same and not clobbered_at then clobbered_at = emu.framecount() end
        text = same and "intact" or "DIFFERS"
    end
    return string.format(
        "frame=%d osMemSize=0x%X magic=0x%08X text=%s%s ticks=%d frames=%d ready=%d init_attempts=%d requests=%d errors=%d",
        emu.framecount(), osmem, magic, text,
        clobbered_at and string.format(" (first differed at frame %d)", clobbered_at) or "",
        u32(ADDR.ticks), u32(ADDR.frames), u32(ADDR.ready), u32(ADDR.init_attempts),
        u32(ADDR.requests), u32(ADDR.errors))
end

while true do
    if emu.framecount() % 30 == 0 then
        local line = snapshot()
        local f = io.open(out, "w")
        if f then f:write(line, "\n"); f:close() end
        gui.text(10, 10, line)
    end
    emu.frameadvance()
end
