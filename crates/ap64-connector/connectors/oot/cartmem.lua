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

local M = {}

local PAGE = 1024

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
