-- Watch the M64P agent spliced into a Paper Mario Randomizer seed.
-- BizHawk: EmuHawk.exe --lua=agentprobe-pmr.lua <spliced rom>. Writes agentprobe-pmr.txt
-- next to itself and draws the numbers on screen.
--
-- AP64's flat agent (agent/build.sh pmr) links at RAM 0x80480000 and is loaded from ROM
-- 0x2B00000. The offsets are nm on agent/pmr/build/agent.elf; re-read them after changing
-- the agent. RDRAM offsets are KSEG0 minus 0x80000000.
local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "agentprobe-pmr.txt"

local ADDR = {
    magic         = 0x481CFC, -- gAgentSegmentMagic, 0x4D363450 once loaded
    init_attempts = 0x481D00, -- s_init_attempts (dormant for good at 16)
    ready         = 0x481D04, -- s_ready
    frames        = 0x481D08, -- s_frames_handled
    ticks         = 0x481D0C, -- s_ticks
    errors        = 0x485CEC, -- s_errors (mem_proto)
    requests      = 0x485CF8, -- s_requests (mem_proto)
}

-- The agent's code in RAM against its copy in ROM. If the randomizer's mod ever
-- writes into the agent, these stop matching, and the first frame it did is kept.
local TEXT_RAM, TEXT_ROM, TEXT_LEN = 0x480000, 0x2B00000, 0x1000
local clobbered_at = nil

local function u32(addr)
    return memory.read_u32_be(addr, "RDRAM")
end

local function snapshot()
    -- Not the RDRAM domain size: BizHawk 3.8 reports 0x800000 even with the Expansion
    -- Slot disabled. osMemSize is what the game, and the agent's guard, actually use.
    local osmem = u32(0x318)
    if osmem < 0x800000 then
        return string.format("frame=%d osMemSize=0x%X (no Expansion Pak: agent must stay unloaded) magic=0x%08X",
            emu.framecount(), osmem, u32(ADDR.magic))
    end
    local magic = u32(ADDR.magic)
    local text = "not loaded"
    if magic == 0x4D363450 then
        local same = memory.hash_region(TEXT_RAM, TEXT_LEN, "RDRAM") == memory.hash_region(TEXT_ROM, TEXT_LEN, "ROM")
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
        if f then
            f:write(line, "\n")
            f:close()
        end
        gui.text(10, 10, line)
    end
    emu.frameadvance()
end
