--[[
AP64's stand-in for a Project64 memory-bridge script: the JavaScript a world's client
loads into Project64's debugger, which listens on localhost and answers a line protocol
with reads and writes of emulated memory. Written against Pokemon Snap's
(nielrenned/PokemonSnap_Archipelago, data/ap_psnap_pj64_adapter_30.js, world v0.8.0),
which does no game logic of its own: the world's ROM does all of the game's work.

The protocol is one request per line, `<id>:<command>`, answered with `<id>:<payload>`:

  romInfo                     the ROM header's name, id, country and CRCs, as JSON
  read <type> <hexaddr> <n>   n bytes from addr as a JSON array of <type> values, or as
                              text for `bytestring`
  dict <json>                 several reads at once: {key: {adr, type, size}} -> {key: [...]}
  write <type> <hexaddr> <v>  a JSON array of <type> values (or text for `bytestring`) to
                              consecutive addresses; "Write successful"

<type> is u8, s8, u16, s16, u32 or s32 here; Project64's float views are not offered,
since no client asks for them. Addresses are the CPU's: KSEG0, KSEG1 or physical, all
mapped onto RDRAM, as Project64's `mem` views do.

A `bytestring` is one character per byte, both ways: upstream reads one with
String.fromCharCode, and writes one with charCodeAt, a byte a UTF-16 unit. The line goes
over the socket as UTF-8 and the client decodes it as UTF-8, so a byte past 0x7F travels
as the character with that code, two bytes on the wire.

What upstream's JavaScript does with a request is matched where the client could see it,
including requests Pokemon Snap Client 0.8.0 never makes: parseInt's leading digits, a
dict entry's `size || 1` and its loop over a size that is not whole, the
`Invalid ...` or `Unknown command` payloads the client raises as script errors, and how a
number is stored in a byte. Where upstream's script would throw instead of answering (an
unknown type, a dict address that is not whole), the request is answered with the nearest
of those payloads, since a script that throws leaves the client waiting for a reply.

What a cart changes:

* Each request is one read_many or one write_many, so it is atomic against the running
  game (docs/integration/host-connector.md section 4). Upstream's script is not either:
  Project64 runs it beside the emulation, and the client makes no request that relies on
  more than one landing together.

* A read of anything outside RDRAM is refused. Upstream's script would return whatever
  the emulator's memory views give for it; the client never asks.

Provided by AP64 (crates/ap64-connector): the global `ap64`, and `json` via require.
]]

local json = require("json")

local RDRAM_SIZE = ap64.rdram_size()

-- Bytes per value and the string.pack format for each type.
local TYPES = {
    u8 = { 1, ">B" },
    s8 = { 1, ">b" },
    u16 = { 2, ">I2" },
    s16 = { 2, ">i2" },
    u32 = { 4, ">I4" },
    s32 = { 4, ">i4" },
}

local function phys(addr, len)
    local p = addr & 0x1FFFFFFF
    if p + len > RDRAM_SIZE then
        error(("address 0x%X+%d is outside the 0x%X bytes of RDRAM the cart has"):format(addr, len, RDRAM_SIZE), 0)
    end
    return p
end

--- What a read of `size` bytes as `ty` covers: upstream steps through `size` a value at a
--- time while the offset is below it, so a size that is not a whole number of values, or not
--- whole at all, reads one more, and one that is not above 0 reads nothing.
local function extent(ty, size)
    local step = ty == "bytestring" and 1 or TYPES[ty][1]
    local n = size > 0 and math.tointeger(math.ceil(size / step)) or 0
    return n, n * step
end

local function decode(ty, data, n)
    local step, fmt = TYPES[ty][1], TYPES[ty][2]
    local out = {}
    for i = 0, n - 1 do
        out[#out + 1] = string.unpack(fmt, data, i * step + 1)
    end
    return out
end

--- A JSON value as JavaScript's ToNumber takes it, for what a list of values can hold.
local function to_number(v)
    if type(v) == "number" then
        return v
    elseif type(v) == "boolean" then
        return v and 1 or 0
    elseif type(v) == "string" then
        local t = v:match("^%s*(.-)%s*$")
        return t == "" and 0 or tonumber(t) or 0 / 0
    end
    return 0 / 0
end

--- A number as it is stored in a value of 8 * step bits: truncated toward zero and wrapped,
--- with NaN and the infinities as 0, as JavaScript stores a number in an integer view.
local function to_bits(v, step)
    local n = to_number(v)
    if n ~= n or n == math.huge or n == -math.huge then
        return 0
    end
    if math.type(n) == "float" then
        n = n < 0 and math.ceil(n) or math.floor(n)
        n = math.tointeger(math.fmod(n, 4294967296.0))
    end
    return n & ((1 << (8 * step)) - 1)
end

local function encode(ty, values)
    local step = TYPES[ty][1]
    local unsigned = ">I" .. step
    local parts = {}
    for i, v in ipairs(values) do
        parts[i] = string.pack(unsigned, to_bits(v, step))
    end
    return table.concat(parts)
end

--- Bytes as upstream's bytestring sends them: each the character with its code, in UTF-8.
local function bytes_to_text(data)
    local out = {}
    for i = 1, #data do
        out[i] = utf8.char(data:byte(i))
    end
    return table.concat(out)
end

--- Text as upstream's bytestring writes it: the low byte of each UTF-16 unit.
local function text_to_bytes(text)
    local out = {}
    for _, c in utf8.codes(text) do
        if c > 0xFFFF then
            c = c - 0x10000
            out[#out + 1] = string.char((0xD800 + (c >> 10)) & 0xFF, (0xDC00 + (c & 0x3FF)) & 0xFF)
        else
            out[#out + 1] = string.char(c & 0xFF)
        end
    end
    return table.concat(out)
end

--- A list of reads, each {type, addr, size}, as one request. Returns each one's values.
local function read_all(reads)
    local regions = {}
    for i, r in ipairs(reads) do
        local _, bytes = extent(r.type, r.size)
        regions[i] = { bytes > 0 and phys(r.addr, bytes) or 0, bytes }
    end
    local wanted = {}
    for _, r in ipairs(regions) do
        if r[2] > 0 then
            wanted[#wanted + 1] = r
        end
    end
    local got = #wanted > 0 and ap64.read_many(wanted) or {}
    -- Back in the order asked, an empty read where nothing was.
    local blocks, j = {}, 0
    for i, r in ipairs(regions) do
        if r[2] > 0 then
            j = j + 1
            blocks[i] = got[j]
        else
            blocks[i] = ""
        end
    end
    local out = {}
    for i, r in ipairs(reads) do
        if r.type == "bytestring" then
            out[i] = bytes_to_text(blocks[i] or "")
        else
            out[i] = decode(r.type, blocks[i], (extent(r.type, r.size)))
        end
    end
    return out
end

--- JavaScript's parseInt(s, base) for base 10 or 16: a sign, for 16 an optional "0x" (the
--- client writes addresses with Python's hex()), and the leading digits; the rest is ignored.
local function parse_int(s, base)
    if not s then
        return nil
    end
    local sign, rest = s:match("^%s*([+-]?)(.*)$")
    if base == 16 then
        rest = rest:gsub("^0[xX]", "")
    end
    local digits = rest:match(base == 16 and "^%x+" or "^%d+")
    if not digits then
        return nil
    end
    local n = tonumber(digits, base)
    return sign == "-" and -n or n
end

local function known(ty)
    return ty == "bytestring" or TYPES[ty] ~= nil
end

local function rom_info()
    local h = ap64.rom_read(0, 0x40)
    local name = h:sub(0x21, 0x34):gsub("%z", ""):match("^%s*(.-)%s*$")
    return json.encode({
        name = name,
        goodName = name,
        id = h:sub(0x3C, 0x3F),
        countryCode = h:byte(0x3F),
        crc1 = string.unpack(">I4", h, 0x11),
        crc2 = string.unpack(">I4", h, 0x15),
    })
end

local function answer(payload)
    local args = {}
    for a in payload:gmatch("%S+") do
        args[#args + 1] = a
    end
    local command = (args[1] or ""):lower()

    if command == "rominfo" then
        return rom_info()
    elseif command == "read" then
        local ty, addr, size = args[2], parse_int(args[3], 16), parse_int(args[4], 10)
        if not addr or not size or size <= 0 or not known(ty) then
            return "Invalid read parameters"
        end
        local v = read_all({ { type = ty, addr = addr, size = size } })[1]
        return ty == "bytestring" and v or json.encode(v)
    elseif command == "dict" then
        local ok, dict = pcall(json.decode, payload:sub(6))
        if not ok or type(dict) ~= "table" then
            return "Invalid Dict JSON Format"
        end
        local keys, reads = {}, {}
        for key, item in pairs(dict) do
            if type(item) ~= "table" then
                item = { adr = item }
            end
            -- `item.size || 1`: 0 is as good as absent.
            local ty, size = item.type or "u8", item.size
            if size == nil or size == 0 or size == false then
                size = 1
            end
            local addr = math.tointeger(item.adr)
            if not addr or type(size) ~= "number" or not known(ty) then
                return "Invalid Dict JSON Format"
            end
            keys[#keys + 1] = key
            reads[#reads + 1] = { type = ty, addr = addr, size = size }
        end
        local values = read_all(reads)
        local out = {}
        for i, key in ipairs(keys) do
            out[key] = values[i]
        end
        return json.encode(out)
    elseif command == "write" then
        local ty, addr = args[2], parse_int(args[3], 16)
        if not addr then
            return "Invalid Target Address"
        end
        if not known(ty) then
            return "Unknown command"
        end
        -- Everything after the address, as upstream takes it: the value may hold spaces.
        local raw = payload:match("^%S+%s+%S+%s+%S+%s+(.*)$") or ""
        local data
        if ty == "bytestring" then
            data = text_to_bytes(raw)
        else
            -- JSON.parse, then Array.isArray: json.lua decodes an object to a table too, so the
            -- text says which it was. A null is ToNumber's 0, and json.lua would leave a hole.
            local list = raw:gsub("%f[%w]null%f[%W]", "0")
            local ok, values = pcall(json.decode, list)
            if not ok or type(values) ~= "table" or not list:match("^%s*%[") then
                return "Value must be a valid JSON Array"
            end
            data = encode(ty, values)
        end
        if #data > 0 then
            ap64.write_many({ { phys(addr, #data), data } })
        end
        return "Write successful"
    end
    return "Unknown command"
end

function handle(message)
    local line = message:gsub("%s+$", "")
    local id, payload = line:match("^([^:]*):(.*)$")
    if not id then
        return "ERR:Invalid message format"
    end
    return id .. ":" .. answer(payload:match("^%s*(.-)%s*$"))
end
