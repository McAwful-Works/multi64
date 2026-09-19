--[[
Copyright (c) 2023 Zunawe

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
]]

--[[
AP64's fork of Archipelago's connector_bizhawk_generic.lua (see UPSTREAM), for a
real N64 running the M64P cart agent instead of BizHawk.

The request protocol is upstream's, unchanged, so Archipelago's BizHawk Client
talks to this exactly as it would to BizHawk: newline-delimited JSON, a list of
requests in and a list of responses out, "VERSION" answered with SCRIPT_VERSION,
and a failed GUARD answering every request after it.

What changed, and why:

* No emulator. Everything BizHawk-specific is gone: the version checks, the
  coroutine and frame callbacks, gui/client/gameinfo/event and the lua_5_3_compat
  shim. The socket is gone too: AP64 owns the TCP port (it binds 43055..43060, as
  upstream did) and calls handle() with each line, sending back what it returns.

* Memory. RDRAM comes from the cart agent through ap64.read_many/write_many, and
  ROM from the cart too, through ap64.rom_read (M64P PEEKROM, cached by AP64: the
  ROM cannot change while the console runs). System Bus maps onto those two. Any
  other domain is refused rather than invented. MEMORY_SIZE for ROM reports the
  window the cart can address, since nothing records the booted image's size.

* HASH. BizHawk hashes the whole ROM; reading all of it over the link would take
  minutes, so this hashes its first 4 KiB, which hold the boot checksum. The
  client only compares it with the first hash it saw, to notice a ROM swap.

* Atomicity. BizHawk runs a whole batch between two frames. Here every RDRAM
  region a batch's READs and GUARDs name is fetched in one call before any
  request runs, so a GUARD and the READs beside it see one snapshot -- the
  property whose absence let a count and a mailbox be read at different moments
  and an item be delivered twice. Writes are overlaid on the snapshot, so a later
  READ in the batch sees them, and sent together before the reply goes out.

* LOCK. A console cannot be paused. LOCK and UNLOCK are acknowledged; each batch
  still sees its own snapshot.

Provided by AP64 (crates/ap64-connector): the global `ap64`, and `json` via require.
]]

local SCRIPT_VERSION = 1

local json = require("json")

local RDRAM_SIZE = ap64.rdram_size()
local ROM_SIZE = ap64.rom_size()
local SYSTEM_BUS_SIZE = 0x100000000

-- Snapshot of the current batch's RDRAM regions: { addr, len, data }.
local snapshot = {}
-- RDRAM writes not yet sent: { addr, data }.
local pending = {}

local RDRAM, ROM = "RDRAM", "ROM"

--- Map a (domain, address) onto RDRAM or ROM.
local function resolve(domain, addr)
    if domain == nil or domain == "RDRAM" then
        return RDRAM, addr
    elseif domain == "ROM" then
        return ROM, addr
    elseif domain == "System Bus" then
        -- KSEG0/KSEG1 and physical alike: RDRAM from 0, cartridge ROM from 0x10000000.
        local phys = addr & 0x1FFFFFFF
        if phys < RDRAM_SIZE then
            return RDRAM, phys
        elseif phys >= 0x10000000 and phys < 0x10000000 + ROM_SIZE then
            return ROM, phys - 0x10000000
        end
        error(("System Bus address 0x%X is neither RDRAM nor ROM"):format(addr), 0)
    end
    error(("memory domain %s is not available on a cart"):format(tostring(domain)), 0)
end

local function check_rdram(addr, len)
    if addr < 0 or len < 0 or addr + len > RDRAM_SIZE then
        error(("RDRAM 0x%X+%d is outside the 0x%X bytes the cart has"):format(addr, len, RDRAM_SIZE), 0)
    end
end

local function rdram_read(addr, len)
    check_rdram(addr, len)
    for _, r in ipairs(snapshot) do
        if addr >= r.addr and addr + len <= r.addr + r.len then
            local off = addr - r.addr
            return r.data:sub(off + 1, off + len)
        end
    end
    -- Not named by the batch up front. Fetch it, and keep it, so a second read of the
    -- same bytes in this batch sees the same instant as the first.
    local data = ap64.read_many({ { addr, len } })[1]
    snapshot[#snapshot + 1] = { addr = addr, len = len, data = data }
    return data
end

--- Overlay a write onto a snapshot region it touches.
local function overlay(r, addr, s)
    local lo = math.max(addr, r.addr)
    local hi = math.min(addr + #s, r.addr + r.len)
    if lo >= hi then
        return
    end
    local off = lo - r.addr
    r.data = r.data:sub(1, off) .. s:sub(lo - addr + 1, hi - addr) .. r.data:sub(off + (hi - lo) + 1)
end

local function read(domain, addr, len)
    local dom, a = resolve(domain, addr)
    if dom == RDRAM then
        return rdram_read(a, len)
    end
    return ap64.rom_read(a, len)
end

local function write(domain, addr, s)
    local dom, a = resolve(domain, addr)
    if dom ~= RDRAM then
        error("ROM is read-only on a cart", 0)
    end
    check_rdram(a, #s)
    pending[#pending + 1] = { a, s }
    for _, r in ipairs(snapshot) do
        overlay(r, a, s)
    end
end

local function flush()
    if #pending > 0 then
        local batch = pending
        pending = {}
        ap64.write_many(batch)
    end
end

--- Decoded length of a base64 string, without decoding it.
local function b64_len(s)
    local body = (s:gsub("[^%w%+/=]", ""))
    local pad = (body:sub(-2) == "==" and 2) or (body:sub(-1) == "=" and 1) or 0
    return (#body // 4) * 3 - pad
end

--- Fetch every RDRAM region the batch's READs and GUARDs name, in one call, and any
--- ROM they name, in another (from AP64's cache when it has it).
local function begin_batch(requests)
    snapshot = {}
    local regions = {}
    local rom_regions = {}
    for _, req in ipairs(requests) do
        if type(req) == "table" and (req.type == "READ" or req.type == "GUARD") then
            local len
            if req.type == "READ" then
                len = tonumber(req.size)
            elseif type(req.expected_data) == "string" then
                len = b64_len(req.expected_data)
            end
            local ok, dom, addr = pcall(resolve, req.domain, tonumber(req.address))
            -- Anything malformed is left for the request itself to fail on, with its
            -- own error, rather than failing the whole batch here.
            if ok and dom == RDRAM and len and len > 0 and addr >= 0 and addr + len <= RDRAM_SIZE then
                regions[#regions + 1] = { addr, len }
            elseif ok and dom == ROM and len and len > 0 and addr >= 0 and addr + len <= ROM_SIZE then
                rom_regions[#rom_regions + 1] = { addr, len }
            end
        end
    end
    if #regions > 0 then
        local blocks = ap64.read_many(regions)
        for i, r in ipairs(regions) do
            snapshot[#snapshot + 1] = { addr = r[1], len = r[2], data = blocks[i] }
        end
    end
    if #rom_regions > 0 then
        -- Only to fill the cache in one exchange; each READ then takes its bytes from it.
        ap64.rom_read_many(rom_regions)
    end
end

local message_interval = 0

local request_handlers = {
    ["PING"] = function (req)
        return { type = "PONG" }
    end,

    ["SYSTEM"] = function (req)
        return { type = "SYSTEM_RESPONSE", value = "N64" }
    end,

    ["PREFERRED_CORES"] = function (req)
        -- A console has no cores to prefer.
        return { type = "PREFERRED_CORES_RESPONSE", value = {} }
    end,

    ["HASH"] = function (req)
        return { type = "HASH_RESPONSE", value = ap64.rom_hash() }
    end,

    ["MEMORY_SIZE"] = function (req)
        local sizes = { RDRAM = RDRAM_SIZE, ROM = ROM_SIZE, ["System Bus"] = SYSTEM_BUS_SIZE }
        local size = sizes[req["domain"] or "RDRAM"]
        if size == nil then
            error(("memory domain %s is not available on a cart"):format(tostring(req["domain"])), 0)
        end
        return { type = "MEMORY_SIZE_RESPONSE", value = size }
    end,

    ["GUARD"] = function (req)
        local expected = ap64.b64decode(req["expected_data"])
        local actual = read(req["domain"], req["address"], #expected)
        return { type = "GUARD_RESPONSE", value = actual == expected, address = req["address"] }
    end,

    ["LOCK"] = function (req)
        return { type = "LOCKED" }
    end,

    ["UNLOCK"] = function (req)
        return { type = "UNLOCKED" }
    end,

    ["READ"] = function (req)
        local data = read(req["domain"], req["address"], req["size"])
        return { type = "READ_RESPONSE", value = ap64.b64encode(data) }
    end,

    ["WRITE"] = function (req)
        write(req["domain"], req["address"], ap64.b64decode(req["value"]))
        return { type = "WRITE_RESPONSE" }
    end,

    ["DISPLAY_MESSAGE"] = function (req)
        -- Shown in AP64's log: a console has no on-screen message queue.
        ap64.message(tostring(req["message"]))
        return { type = "DISPLAY_MESSAGE_RESPONSE" }
    end,

    ["SET_MESSAGE_INTERVAL"] = function (req)
        message_interval = req["value"]
        return { type = "SET_MESSAGE_INTERVAL_RESPONSE" }
    end,

    ["default"] = function (req)
        return { type = "ERROR", err = "Unknown command: " .. tostring(req["type"]) }
    end,
}

local function process_request(req)
    return (request_handlers[req["type"]] or request_handlers["default"])(req)
end

--- One line from the client in, one line out (without the newline).
function handle(message)
    if message == "VERSION" then
        return tostring(SCRIPT_VERSION)
    end
    local data = json.decode(message)
    begin_batch(data)
    local res = {}
    local failed_guard_response = nil
    for i, req in ipairs(data) do
        if failed_guard_response ~= nil then
            res[i] = failed_guard_response
        else
            local status, response = pcall(process_request, req)
            if status then
                res[i] = response
                -- If the GUARD validation failed, skip the remaining commands.
                if response["type"] == "GUARD_RESPONSE" and not response["value"] then
                    failed_guard_response = response
                end
            else
                if type(response) ~= "string" then response = "Unknown error" end
                res[i] = { type = "ERROR", err = response }
            end
        end
    end
    -- Writes land before the client hears they were made.
    flush()
    return json.encode(res)
end
