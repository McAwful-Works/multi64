-- Measure whether an Archipelago connector script could be driven over a cart link.
-- BizHawk: load this instead of the connector, with TARGET below pointing at it.
-- Writes connector-trace.txt next to itself.
-- Lua 5.1: BizHawk's NLua core is 5.1, so no `//` and nothing else 5.3 added.
--
-- A world whose connector only answers READ/WRITE for its client can be driven by a
-- generic cart stand-in: the stand-in replays exactly what the client asked for. A world
-- whose connector *also* does its own work on a frame callback cannot, because that work
-- never appears on the wire and so is never replayed. Banjo-Tooie cost a week to learn
-- this: 39 setSetting calls at slot time were skipped and the ROM ran uninitialized.
--
-- This tool separates the two. It proxies the memory API, and wraps every callback the
-- target registers, so each access is tagged with the phase it happened in:
--
--   client   -- outside any callback: the connector serving a socket request
--   <event>  -- inside a callback the target registered, e.g. onframestart
--
-- Accesses tagged `client` are replayable. Everything else is work a stand-in would have
-- to reimplement, and the report is a budget for doing so.
--
-- Cost is reported as coalesced regions, not raw accesses, because that is what a cart
-- link charges for. Neighboring addresses ride in one request; a scattered set does not.
-- Multiply regions-per-frame by your measured round trip (~67 ms on the SC64 link, see
-- ../connectors/bt/cartmem.lua) to get the real per-frame cost, and compare it against a
-- 16.7 ms frame.
--
-- What this does NOT show: whether a read's *address* came from a previous read. A
-- dependent pointer chain costs one round trip per link no matter how well it coalesces,
-- and it will look cheap here. Read the target for `while` loops that follow a next
-- pointer before trusting a low number.

-- BizHawk reports `source` as "main" for the script named by --lua=, so this is empty in
-- that case and only resolves when the file is opened from the Lua Console or dofile'd.
-- Both settings below therefore accept an absolute path; give one when using --lua=.
local here = debug.getinfo(1, "S").source:match("^@(.*[/\\])") or ""
-- Report path. A wrapper may set CONNECTOR_TRACE_OUT to keep the report with the run
-- rather than in this folder, which matters when the tools folder is a git checkout.
local out = CONNECTOR_TRACE_OUT or (here .. "connector-trace.txt")

-- The connector to measure. Give a full path, or a bare name to load from this folder.
-- A wrapper script may set CONNECTOR_TRACE_TARGET before loading this file instead of
-- editing it, which is how the self-test points it at a stub.
local TARGET = CONNECTOR_TRACE_TARGET or "goldeneye_ap.lua"

-- Two addresses closer than this are fetched as one region. Matches COALESCE_GAP in
-- ap64-connector's cartmem shims, so the region counts here mean the same thing there.
local COALESCE_GAP = 64
-- Spec 4 caps one request at this many regions; more than this is a second round trip.
local MAX_REGIONS = 32
-- Report every this many frames. The file is rewritten each time, so it is readable
-- while the run is still going.
local REPORT_EVERY = 60

--------------------------------------------------------------------------------
-- Recording
--------------------------------------------------------------------------------

local phase = "client"            -- current frame phase, see header
local frames = 0
local reads, writes = {}, {}      -- phase -> { addr -> true } for this frame
local ops = {}                    -- phase -> { reads = n, writes = n } for this frame
local seen_phases = {}            -- every phase name seen, in first-seen order
local stats = {}                  -- phase -> accumulated per-frame samples

local function phase_slot(tbl)
    if tbl[phase] == nil then tbl[phase] = {} end
    return tbl[phase]
end

local function note_phase(name)
    for i = 1, #seen_phases do if seen_phases[i] == name then return end end
    seen_phases[#seen_phases + 1] = name
end

-- Forward declaration: rolling a frame needs the stats machinery defined below.
local roll_frame

-- Frames are detected here rather than on a frame hook. A hook would have to run after
-- the target's own end-of-frame handler to see its work, and BizHawk does not promise
-- handlers fire in registration order -- so the sample would silently miss exactly the
-- work this tool exists to measure. Reading the frame counter on each access needs no
-- such promise.
local last_frame = nil

local function record(addr, len, is_write)
    if type(addr) ~= "number" then return end
    local now = emu.framecount()
    if last_frame == nil then
        last_frame = now
    elseif now ~= last_frame then
        roll_frame()
        last_frame = now
    end
    note_phase(phase)
    local set = phase_slot(is_write and writes or reads)
    for i = 0, (len or 1) - 1 do set[addr + i] = true end
    local o = ops[phase]
    if o == nil then o = { reads = 0, writes = 0 }; ops[phase] = o end
    if is_write then o.writes = o.writes + 1 else o.reads = o.reads + 1 end
end

--- Coalesce an address set into { addr, len } runs, largest gap COALESCE_GAP.
local function regions_for(set)
    local addrs = {}
    for a in pairs(set) do addrs[#addrs + 1] = a end
    table.sort(addrs)
    local n = 0
    local last = nil
    for i = 1, #addrs do
        if last == nil or addrs[i] - last > COALESCE_GAP then n = n + 1 end
        last = addrs[i]
    end
    return n, #addrs
end

--------------------------------------------------------------------------------
-- Proxies over the memory API
--------------------------------------------------------------------------------

-- Width implied by the accessor's name; read_bytes_as_array takes its length as an
-- argument instead, and is handled separately.
local function width_of(name)
    if name:find("8") then return 1 end
    if name:find("16") then return 2 end
    if name:find("32") then return 4 end
    if name:find("byte") then return 1 end
    return 4
end

local READERS = {
    "read_u8", "read_u16_be", "read_u16_le", "read_u24_be", "read_u32_be", "read_u32_le",
    "read_s8", "read_s16_be", "read_s16_le", "read_s32_be", "read_s32_le", "readbyte",
}
local WRITERS = {
    "write_u8", "write_u16_be", "write_u16_le", "write_u24_be", "write_u32_be",
    "write_u32_le", "write_s8", "write_s16_be", "write_s16_le", "write_s32_be",
    "write_s32_le", "writebyte",
}

local function wrap_table(t)
    if t == nil then return end
    for _, name in ipairs(READERS) do
        local orig = t[name]
        if orig then
            local w = width_of(name)
            t[name] = function(addr, ...) record(addr, w, false); return orig(addr, ...) end
        end
    end
    for _, name in ipairs(WRITERS) do
        local orig = t[name]
        if orig then
            local w = width_of(name)
            t[name] = function(addr, ...) record(addr, w, true); return orig(addr, ...) end
        end
    end
    local rba = t.read_bytes_as_array
    if rba then
        t.read_bytes_as_array = function(addr, len, ...)
            record(addr, len, false); return rba(addr, len, ...)
        end
    end
    local wba = t.write_bytes_as_array
    if wba then
        t.write_bytes_as_array = function(addr, arr, ...)
            record(addr, arr and #arr or 1, true); return wba(addr, arr, ...)
        end
    end
end

wrap_table(mainmemory)
wrap_table(memory)

--------------------------------------------------------------------------------
-- Phase tagging: wrap every callback the target registers
--------------------------------------------------------------------------------

-- The target registers its handlers when we load it, below. Wrapping the registrars
-- first means every handler runs with `phase` set to the event that fired it, and
-- anything else -- the socket poll driving the connector -- stays tagged `client`.
local EVENTS = { "onframestart", "onframeend", "onloadstate", "onsavestate", "onexit" }
local raw_event = {}
for _, ev in ipairs(EVENTS) do
    local orig = event[ev]
    raw_event[ev] = orig
    if orig then
        event[ev] = function(cb, ...)
            return orig(function(...)
                local prev = phase
                phase = ev
                local ok, err = pcall(cb, ...)
                phase = prev
                if not ok then error(err, 0) end
            end, ...)
        end
    end
end

--------------------------------------------------------------------------------
-- Reporting
--------------------------------------------------------------------------------

local report

roll_frame = function()
    for _, p in ipairs(seen_phases) do
        local s = stats[p]
        if s == nil then
            s = { frames = 0, regions = {}, r_ops = 0, w_ops = 0, max_regions = 0,
                  max_addrs = 0, over_cap = 0 }
            stats[p] = s
        end
        local rset = reads[p] or {}
        local wset = writes[p] or {}
        local merged = {}
        for a in pairs(rset) do merged[a] = true end
        for a in pairs(wset) do merged[a] = true end
        local nregions, naddrs = regions_for(merged)
        if naddrs > 0 then
            s.frames = s.frames + 1
            s.regions[#s.regions + 1] = nregions
            if nregions > s.max_regions then s.max_regions = nregions end
            if naddrs > s.max_addrs then s.max_addrs = naddrs end
            if nregions > MAX_REGIONS then s.over_cap = s.over_cap + 1 end
            local o = ops[p] or { reads = 0, writes = 0 }
            s.r_ops = s.r_ops + o.reads
            s.w_ops = s.w_ops + o.writes
        end
    end
    reads, writes, ops = {}, {}, {}
    frames = frames + 1
    if frames % REPORT_EVERY == 0 then report() end
end

local function percentile(list, q)
    if #list == 0 then return 0 end
    local copy = {}
    for i = 1, #list do copy[i] = list[i] end
    table.sort(copy)
    local i = math.floor(q * (#copy - 1)) + 1
    return copy[i]
end

report = function()
    local lines = {}
    lines[#lines + 1] = ("target=%s frames=%d coalesce_gap=%d region_cap=%d"):format(
        TARGET, frames, COALESCE_GAP, MAX_REGIONS)
    lines[#lines + 1] = ""
    lines[#lines + 1] = "phase          frames   reads  writes  regions/frame med/p95/max  over-cap"
    for _, p in ipairs(seen_phases) do
        local s = stats[p]
        if s and s.frames > 0 then
            lines[#lines + 1] = ("  %-12s %6d %7d %7d  %5d /%4d /%4d     %6d"):format(
                p, s.frames, s.r_ops, s.w_ops,
                percentile(s.regions, 0.5), percentile(s.regions, 0.95), s.max_regions,
                s.over_cap)
        end
    end
    lines[#lines + 1] = ""
    lines[#lines + 1] = "A cart stand-in replays the `client` row for free: the requests are on the wire."
    lines[#lines + 1] = "Every other row is work it would have to reimplement, and pay round trips for:"
    lines[#lines + 1] = ""
    for _, p in ipairs(seen_phases) do
        local s = stats[p]
        if p ~= "client" and s and s.frames > 0 then
            local med = percentile(s.regions, 0.5)
            lines[#lines + 1] = ("  %-12s %d regions/frame median -> %d ms/frame at 67 ms/round trip (frame budget 16.7 ms)"):format(
                p, med, med * 67)
        end
    end
    local f = io.open(out, "w")
    if f then f:write(table.concat(lines, "\n"), "\n"); f:close() end
    return lines[1]
end

--------------------------------------------------------------------------------
-- Run the target under the proxies
--------------------------------------------------------------------------------

-- The last partial frame is never rolled by an access, so flush it here. This registers
-- through the unwrapped registrar saved above, so the flush is not itself tagged.
raw_event.onexit(function()
    roll_frame()
    report()
end)

local path = TARGET:find("[/\\]") and TARGET or (here .. TARGET)
print("connector-trace: loading " .. path)
dofile(path)
