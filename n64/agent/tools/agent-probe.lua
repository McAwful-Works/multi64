-- Watch the M64P agent inside a ROM running in BizHawk.
-- BizHawk: EmuHawk.exe --lua=agent-probe.lua <rom>. Writes agent-probe.txt next to itself
-- and draws the numbers on screen.
--
-- Fill in the four settings from your build. For a flat image they are all in
-- templates/link-flat.sh's layout.env; for an agent linked into a game's own build,
-- take the addresses from its symbol map (VRAM of the agent's section, ROM offset it
-- is loaded from, and nm offsets of the counters relative to that VRAM).
local AGENT_VRAM = 0x80480000      -- AGENT_VRAM
local AGENT_ROM = 0xC00000         -- AGENT_ROM (only used for the code-intact check)
local AGENT_MIN_RAM = 0x800000     -- AGENT_MIN_RAM
local OFF = {                      -- OFF_* from layout.env
    magic         = 0x1100,
    init_attempts = 0x1104,
    ready         = 0x1108,
    frames        = 0x110C,
    ticks         = 0x1110,
    errors        = 0x50E8,
    requests      = 0x50F4,
}

local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "agent-probe.txt"
local base = AGENT_VRAM - 0x80000000
local TEXT_LEN = 0x1000
local clobbered_at = nil

local function u32(offset) return memory.read_u32_be(base + offset, "RDRAM") end

local function snapshot()
    -- osMemSize, not the RDRAM domain size: some BizHawk versions report 8 MB even
    -- with the Expansion Slot disabled.
    local osmem = memory.read_u32_be(0x318, "RDRAM")
    local magic = u32(OFF.magic)
    if osmem < AGENT_MIN_RAM then
        return string.format("frame=%d osMemSize=0x%X below AGENT_MIN_RAM: agent must stay unloaded, magic=0x%08X",
            emu.framecount(), osmem, magic)
    end
    local text = "not loaded"
    if magic == 0x4D363450 then
        -- The agent's first 4 KB of code in RAM against its copy in ROM. If anything
        -- in the game writes into the agent, these stop matching.
        local same = memory.hash_region(base, TEXT_LEN, "RDRAM") == memory.hash_region(AGENT_ROM, TEXT_LEN, "ROM")
        if not same and not clobbered_at then clobbered_at = emu.framecount() end
        text = same and "intact" or "DIFFERS"
    end
    return string.format(
        "frame=%d osMemSize=0x%X magic=0x%08X text=%s%s ticks=%d frames=%d ready=%d init_attempts=%d requests=%d errors=%d",
        emu.framecount(), osmem, magic, text,
        clobbered_at and string.format(" (first differed at frame %d)", clobbered_at) or "",
        u32(OFF.ticks), u32(OFF.frames), u32(OFF.ready), u32(OFF.init_attempts),
        u32(OFF.requests), u32(OFF.errors))
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
