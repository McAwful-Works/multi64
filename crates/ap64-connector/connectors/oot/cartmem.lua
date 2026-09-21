-- RDRAM for AP64's fork of connector_oot.lua, through the cart (ap64.read_many / write_many).
--
-- The connector makes 400+ scattered reads a poll, every location helper reading memory
-- directly. A cart round trip costs ~67 ms whatever its size, so they cannot each be one.
-- Reads are therefore served from 1 KiB pages, and the pages a poll touched are
-- remembered: the next poll fetches that whole set in one call before anything runs, so a
-- steady-state poll is one exchange, or two if the set is larger than one M64P request.
--
-- The cache lives for one poll (begin_poll). Writes update it at once, so later reads in
-- the poll see them, and are sent together by flush(), before the reply goes out.
--
-- refresh() re-reads a few regions in one request and overrides the cache with them. The
-- connector uses it where two values must come from the same instant: the received-item
-- count and the item mailbox. Read apart, the game can consume the mailbox between the
-- two reads, and the same item is delivered twice.
--
-- The one place this serves something other than the bytes in memory now is the transient
-- slot below, and it is what stops a check waiting for a scene change.

local M = {}

local PAGE = 1024

-- OoT commits scene flags to the save context on a scene transition. Until then the only
-- sign a check was collected is this 4-byte slot -- scene, type, 0, id -- holding the most
-- recent flag-set event, which connector.lua reads once per scan (check_temp_context).
-- The next event overwrites it. An emulator reads it every frame; a cart poll is ~67-100 ms
-- and misses what happened in between, and the check then waits until you leave the room.
--
-- So AP64 watches the slot. An agent that can do it follows the slot on the console every
-- frame and sends what it saw back on responses already going out; an older one is sampled
-- from the host instead, alongside each request and on idle time. Either way each change is
-- queued, and the read below hands back the oldest one the connector has not been shown,
-- and live bytes once the queue has drained. The types are the ones a
-- check_temp_context call site is ever called with -- chests (0x01), freestanding (0x02),
-- great fairies (0x05), and 0x00 for the one-offs -- so an event no call site could match
-- never takes a scan's place in the queue. See ap64_cart::watch.
local TEMP_CONTEXT = 0x40002C
local TEMP_CONTEXT_LEN = 4
ap64.watch(TEMP_CONTEXT, TEMP_CONTEXT_LEN, 1, { 0x00, 0x01, 0x02, 0x05 })

local pages = {}      -- page number -> string of PAGE bytes, this poll
local touched = {}    -- page number -> true, this poll
local learned = {}    -- page numbers the last poll touched
local overlay = {}    -- address -> byte, from refresh(), this poll
local pending = {}    -- { addr, string } writes not yet sent

local function fetch(list)
    local regions = {}
    for i, p in ipairs(list) do
        regions[i] = { p * PAGE, PAGE }
    end
    local blocks = ap64.read_many(regions)
    for i, p in ipairs(list) do
        pages[p] = blocks[i]
    end
end

--- Start a poll: forget the last poll's bytes and fetch what it touched, in one call.
function M.begin_poll()
    pages, overlay = {}, {}
    local want = {}
    for p in pairs(touched) do
        want[#want + 1] = p
    end
    table.sort(want)
    learned, touched = want, {}
    if #want > 0 then
        fetch(want)
    end
end

local function byte_at(a)
    local o = overlay[a]
    if o then
        return o
    end
    local p = a // PAGE
    touched[p] = true
    local page = pages[p]
    if not page then
        fetch({ p })
        page = pages[p]
    end
    return string.byte(page, a % PAGE + 1)
end

local function read_be(a, n)
    local v = 0
    for i = 0, n - 1 do
        v = (v << 8) | byte_at(a + i)
    end
    return v
end

local function set_byte(a, b)
    overlay[a] = b
    local p = a // PAGE
    local page = pages[p]
    if page then
        local o = a % PAGE
        pages[p] = page:sub(1, o) .. string.char(b) .. page:sub(o + 2)
    end
end

local function write_be(a, n, v)
    local s = {}
    for i = n - 1, 0, -1 do
        s[i + 1] = string.char(v & 0xFF)
        v = v >> 8
    end
    local bytes = table.concat(s)
    pending[#pending + 1] = { a, bytes }
    for i = 1, n do
        set_byte(a + i - 1, string.byte(bytes, i))
    end
end

--- Read these regions ({addr, len} each) again, together, and serve them from now on.
function M.refresh(regions)
    local blocks = ap64.read_many(regions)
    for i, r in ipairs(regions) do
        for k = 1, r[2] do
            overlay[r[1] + k - 1] = string.byte(blocks[i], k)
        end
    end
end

--- Every change to the watched slot that the connector has not been shown, oldest first,
--- with the slot's live bytes last. Each is a 0-indexed table of TEMP_CONTEXT_LEN bytes,
--- shaped as connector.lua's check_temp_context expects.
---
--- A scan is shown all of them, not one: the queue fills at the rate the game sets flags,
--- which in a dungeon is far faster than a cart poll, and one per scan let a real check be
--- pushed out of the queue before any scan saw it. That is "the chest only registered when
--- I left the room". Draining it empties the queue every poll instead, and the scan can
--- recognise several checks at once, which is what an emulator reading every frame does.
---
--- A cleared slot is dropped here too. Neither the agent nor the host's own sampling
--- queues one, so this only guards the day something else does: all zeros is never a
--- check, and it matches any call site whose expected values happen to be all zero.
function M.temp_events()
    local out = {}
    while true do
        local queued = ap64.take_watched()
        if not queued then
            break
        end
        -- Taking an event consumes it, so anything but the bytes the watch promises has
        -- already lost one. Carrying on would hide that.
        if #queued ~= TEMP_CONTEXT_LEN then
            error(("ap64.take_watched returned %d bytes, want %d")
                :format(#queued, TEMP_CONTEXT_LEN))
        end
        local event, cleared = {}, true
        for i = 0, TEMP_CONTEXT_LEN - 1 do
            event[i] = string.byte(queued, i + 1)
            if event[i] ~= 0 then
                cleared = false
            end
        end
        if not cleared then
            out[#out + 1] = event
        end
    end
    local live = {}
    for i = 0, TEMP_CONTEXT_LEN - 1 do
        live[i] = byte_at(TEMP_CONTEXT + i)
    end
    out[#out + 1] = live
    return out
end

--- Send this poll's writes, in one call.
function M.flush()
    if #pending > 0 then
        local batch = pending
        pending = {}
        ap64.write_many(batch)
    end
end

-- The BizHawk functions connector_oot.lua calls, over the above.
local mainmemory = {}
function mainmemory.read_u8(a) return byte_at(a) end
function mainmemory.readbyte(a) return byte_at(a) end
function mainmemory.read_u16_be(a) return read_be(a, 2) end
function mainmemory.read_u24_be(a) return read_be(a, 3) end
function mainmemory.read_u32_be(a) return read_be(a, 4) end
-- BizHawk returns a 0-INDEXED table here, and the connector's bytes_to_string relies on
-- it (`for i=0,#(bytes)`). Do not make it 1-indexed.
function mainmemory.readbyterange(a, len)
    local t = {}
    for i = 0, len - 1 do
        t[i] = byte_at(a + i)
    end
    return t
end
function mainmemory.write_u8(a, v) write_be(a, 1, v) end
function mainmemory.writebyte(a, v) write_be(a, 1, v) end
function mainmemory.write_u16_be(a, v) write_be(a, 2, v) end
function mainmemory.write_u24_be(a, v) write_be(a, 3, v) end
function mainmemory.write_u32_be(a, v) write_be(a, 4, v) end
M.mainmemory = mainmemory

return M
