-- RDRAM for AP64's fork of connector_banjo_tooie_bizhawk.lua, through the cart
-- (ap64.read_many / ap64.write_many).
--
-- Banjo-Tooie's connector cannot run a byte at a time over a cart, and the reason is not
-- the number of locations. It is the pointer chain. Every accessor resolves its own
-- pointer first, and each getter is a double dereference:
--
--     getRealFlagPointer() = deref(deref(0x400000) + real_flags)
--
-- so BTHACK:checkRealFlag() costs two u32 reads before it reads the flag byte. A poll
-- checks 562 locations, which is about 1,700 reads, and at ~67 ms a round trip that is
-- not a poll, it is a minute and a half.
--
-- Cached it collapses, because those 1,700 reads land on very few addresses. The anchor at
-- 0x400000 is read every single time and never changes. The struct's pointer table is
-- 0x28-odd bytes read over and over. And the 562 locations share only 90 distinct byte
-- offsets spanning 0x03..0x9E -- a 156-byte window, one region.
--
-- So this is the learned-set shape rather than OoT's fixed pages: it remembers the exact
-- addresses a poll touched and refetches that set, coalesced into runs, at the start of the
-- next one. Nothing here knows a Banjo-Tooie address, which matters more than usual since
-- every one of them is relative to a pointer resolved at runtime -- a fixed page map would
-- be wrong the moment the BTHACK moved its block.
--
-- Writes are queued and sent by flush() before the reply goes out, and overlay the cache at
-- once so later reads in the same poll see them. Upstream depends on that: setPCDeath reads
-- the counter, writes counter+1, and SendToBTClient reads it again in the same pass.

local M = {}

-- Two addresses closer than this are fetched as one region: a few wasted bytes cost
-- nothing next to a second round trip. The flag window's 90 offsets inside 156 bytes
-- coalesce into a single run at this gap.
local COALESCE_GAP = 64
-- Spec 4 caps a request at 32 regions, so the learned set is coalesced and then capped;
-- anything past the cap is fetched on demand and learned again next poll.
local MAX_REGIONS = 32

local cache = {}      -- address -> byte, this poll
local touched = {}    -- address -> true, this poll
local learned = {}    -- addresses the last poll touched
local pending = {}    -- { addr, string } writes, this poll

--- Coalesce a set of addresses into { addr, len } runs, largest gap COALESCE_GAP.
local function runs_for(addrs)
    local sorted = {}
    for a in pairs(addrs) do
        sorted[#sorted + 1] = a
    end
    table.sort(sorted)
    local runs = {}
    local i = 1
    while i <= #sorted do
        local lo = sorted[i]
        local hi = lo
        while i + 1 <= #sorted and sorted[i + 1] - hi <= COALESCE_GAP do
            i = i + 1
            hi = sorted[i]
        end
        runs[#runs + 1] = { lo, hi - lo + 1 }
        i = i + 1
    end
    return runs
end

local function store(addr, block)
    for k = 1, #block do
        cache[addr + k - 1] = block:byte(k)
    end
end

--- Fetch what the last poll read, in one call, and start a new poll.
function M.begin_poll()
    cache = {}
    touched = {}
    pending = {}

    local runs = runs_for(learned)
    if #runs == 0 then
        return
    end
    while #runs > MAX_REGIONS do
        table.remove(runs)
    end
    local blocks = ap64.read_many(runs)
    for i, r in ipairs(runs) do
        store(r[1], blocks[i])
    end
end

local function fetch(addr, len)
    local block = ap64.read_many({ { addr, len } })[1]
    store(addr, block)
    return block
end

--- Ensure len bytes from addr are cached, and mark them read.
local function need(addr, len)
    local missing = false
    for k = 0, len - 1 do
        touched[addr + k] = true
        if cache[addr + k] == nil then
            missing = true
        end
    end
    if missing then
        fetch(addr, len)
    end
end

function M.readbyte(addr)
    need(addr, 1)
    return cache[addr]
end

function M.read_u16_be(addr)
    need(addr, 2)
    return cache[addr] * 0x100 + cache[addr + 1]
end

function M.read_u32_be(addr)
    need(addr, 4)
    return cache[addr] * 0x1000000 + cache[addr + 1] * 0x10000
        + cache[addr + 2] * 0x100 + cache[addr + 3]
end

--- Queue bytes, and show them to any read later in this poll.
local function put(addr, bytes)
    for k = 1, #bytes do
        cache[addr + k - 1] = bytes:byte(k)
        touched[addr + k - 1] = true
    end
    pending[#pending + 1] = { addr, bytes }
end

function M.writebyte(addr, value)
    put(addr, string.char(value % 0x100))
end

function M.write_u16_be(addr, value)
    value = value % 0x10000
    put(addr, string.char(math.floor(value / 0x100), value % 0x100))
end

function M.write_u32_be(addr, value)
    value = value % 0x100000000
    put(addr, string.char(
        math.floor(value / 0x1000000) % 0x100,
        math.floor(value / 0x10000) % 0x100,
        math.floor(value / 0x100) % 0x100,
        value % 0x100))
end

--- Send this poll's writes, and remember what it read for the next one.
function M.flush()
    if #pending > 0 then
        ap64.write_many(pending)
        pending = {}
    end
    learned = touched
end

--- What upstream calls `mainmemory`. The fork points its one global at this.
M.mainmemory = {
    readbyte = M.readbyte,
    read_u16_be = M.read_u16_be,
    read_u32_be = M.read_u32_be,
    writebyte = M.writebyte,
    write_u16_be = M.write_u16_be,
    write_u32_be = M.write_u32_be,
}

return M
