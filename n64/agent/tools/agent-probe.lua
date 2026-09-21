-- Watch the M64P agent inside a ROM running in BizHawk.
-- BizHawk: EmuHawk.exe --lua=agent-probe.lua <rom>. Writes agent-probe.txt next to itself
-- and draws the numbers on screen.
-- Lua 5.1: BizHawk's NLua core is 5.1, so no `//` and nothing else 5.3 added.
--
-- Every address comes from the build's own layout.env, which both builders write:
-- crates/ap64-core/agent/build.sh (per game, into profiles/<game>/) and
-- templates/link-flat.sh (a flat image, into its output directory). Nothing here is
-- hardcoded, because hardcoded offsets go stale silently: this script used to carry a
-- table from a build whose magic sat at +0x1100, long after it had moved to +0x2468, and
-- a probe reading the wrong words looks exactly like a probe reading the right ones.
--
-- Set GAME to read a profile's layout.env, or LAYOUT to point anywhere else.

local GAME = "cv64"   -- profiles/<GAME>/layout.env, relative to this file in the repo
local LAYOUT = nil    -- or an explicit path, which wins over GAME

local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
local out = here .. "agent-probe.txt"

--- Every KEY=0x... in a layout.env, or nil and a reason.
local function read_layout(path)
    local f = io.open(path, "r")
    if not f then
        return nil, "no layout.env at " .. path
    end
    local env = {}
    for line in f:lines() do
        local k, v = line:match("^([A-Z_]+)=(%S+)")
        if k and v then
            env[k] = tonumber(v) or tonumber(v, 16)
        end
    end
    f:close()
    return env
end

local function load_settings()
    local tried = {}
    local paths = {}
    if LAYOUT then
        paths[#paths + 1] = LAYOUT
    end
    paths[#paths + 1] = here .. "layout.env"
    if GAME then
        paths[#paths + 1] = here .. "../../../crates/ap64-core/profiles/" .. GAME .. "/layout.env"
    end
    for _, p in ipairs(paths) do
        local env, why = read_layout(p)
        if env then
            return env, p
        end
        tried[#tried + 1] = why
    end
    return nil, table.concat(tried, "; ")
end

local env, from = load_settings()
if not env then
    -- Loudly, and forever: wrong numbers on screen are worse than none.
    while true do
        gui.text(10, 10, "agent-probe: " .. from)
        gui.text(10, 26, "set GAME or LAYOUT at the top of agent-probe.lua")
        emu.frameadvance()
    end
end

-- A layout.env from before the OFF_* block would leave these nil and read address 0.
local NEEDED = {
    "AGENT_VRAM", "AGENT_ROM", "AGENT_MIN_RAM",
    "OFF_MAGIC", "OFF_INIT_ATTEMPTS", "OFF_READY", "OFF_FRAMES_HANDLED",
    "OFF_TICKS", "OFF_REQUESTS", "OFF_ERRORS",
}
local missing = {}
for _, k in ipairs(NEEDED) do
    if not env[k] then
        missing[#missing + 1] = k
    end
end
if #missing > 0 then
    while true do
        gui.text(10, 10, "agent-probe: " .. from)
        gui.text(10, 26, "layout.env is missing " .. table.concat(missing, ", "))
        gui.text(10, 42, "rebuild the agent: build.sh writes these")
        emu.frameadvance()
    end
end

local AGENT_VRAM = env.AGENT_VRAM
local AGENT_ROM = env.AGENT_ROM          -- only used for the code-intact check
local AGENT_MIN_RAM = env.AGENT_MIN_RAM
local OFF = {
    magic         = env.OFF_MAGIC,
    init_attempts = env.OFF_INIT_ATTEMPTS,
    ready         = env.OFF_READY,
    frames        = env.OFF_FRAMES_HANDLED,
    ticks         = env.OFF_TICKS,
    errors        = env.OFF_ERRORS,
    requests      = env.OFF_REQUESTS,
    last_error    = env.OFF_LAST_ERROR,  -- optional: older layout.env has no such line
}

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
    -- The code, not just the count: an error total says something is wrong, and which
    -- one says what. 0x04 is E_RANGE, which a host asking for a KSEG0 address gets.
    local errors = u32(OFF.errors)
    local last = ""
    if OFF.last_error and errors > 0 then
        last = string.format("(last=0x%02X)", math.floor(u32(OFF.last_error) / 0x1000000))
    end
    return string.format(
        "frame=%d osMemSize=0x%X magic=0x%08X text=%s%s ticks=%d frames=%d ready=%d init_attempts=%d requests=%d errors=%d%s",
        emu.framecount(), osmem, magic, text,
        clobbered_at and string.format(" (first differed at frame %d)", clobbered_at) or "",
        u32(OFF.ticks), u32(OFF.frames), u32(OFF.ready), u32(OFF.init_attempts),
        u32(OFF.requests), errors, last)
end

local banner = string.format("agent-probe: %s (agent at 0x%08X)", from, AGENT_VRAM)
while true do
    if emu.framecount() % 30 == 0 then
        local line = snapshot()
        local f = io.open(out, "w")
        if f then f:write(line, "\n"); f:close() end
        gui.text(10, 10, banner)
        gui.text(10, 26, line)
    end
    emu.frameadvance()
end
