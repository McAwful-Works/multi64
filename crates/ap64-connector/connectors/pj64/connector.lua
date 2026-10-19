--[[
AP64's stand-in for a Project64 memory-bridge script: the JavaScript a world's client
loads into Project64's debugger, which listens on localhost and answers a line protocol
with reads and writes of emulated memory. Written against Pokemon Snap's
(nielrenned/PokemonSnap_Archipelago, data/ap_psnap_pj64_adapter_30.js, world v0.8.0),
which does no game logic of its own: the world's ROM does all of the game's work.

The protocol is one request per line, `<id>:<command>`, answered with `<id>:<payload>`:

  romInfo                     the ROM header's name, id, country and CRCs, as JSON
  read <type> <hexaddr> <n>   n bytes from addr as a JSON array of <type> values, or the
                              raw bytes for `bytestring`
  dict <json>                 several reads at once: {key: {adr, type, size}} -> {key: [...]}
  write <type> <hexaddr> <v>  a JSON array of <type> values (or the raw bytes for
                              `bytestring`) to consecutive addresses; "Write successful"

<type> is u8, s8, u16, s16, u32 or s32 here; Project64's float views are not offered,
since no client asks for them. A request that cannot be parsed is answered as upstream
answers it, with an `Invalid ...` or `Unknown command` payload, which the client raises
as a script error. Addresses are the CPU's: KSEG0, KSEG1 or physical, all mapped onto
RDRAM, as Project64's `mem` views do.

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
--- time, so a size that is not a whole number of values reads one more.
local function extent(ty, size)
    if ty == "bytestring" then
        return size, size
    end
    local step = TYPES[ty][1]
    local n = (size + step - 1) // step
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

local function encode(ty, values)
    local step = TYPES[ty][1]
    local mask = (1 << (8 * step)) - 1
    local unsigned = ">I" .. step
    local parts = {}
    for i, v in ipairs(values) do
        if math.type(v) ~= "integer" then
            if type(v) ~= "number" or v ~= math.floor(v) then
                error("value " .. tostring(v) .. " is not a whole number", 0)
            end
            v = math.tointeger(v)
        end
        parts[i] = string.pack(unsigned, v & mask)
    end
    return table.concat(parts)
end

--- A list of reads, each {type, addr, size}, as one request. Returns each one's values.
local function read_all(reads)
    local regions = {}
    for i, r in ipairs(reads) do
        local _, bytes = extent(r.type, r.size)
        regions[i] = { phys(r.addr, bytes), bytes }
    end
    local blocks = #regions > 0 and ap64.read_many(regions) or {}
    local out = {}
    for i, r in ipairs(reads) do
        if r.type == "bytestring" then
            out[i] = blocks[i]
        else
            out[i] = decode(r.type, blocks[i], (extent(r.type, r.size)))
        end
    end
    return out
end

--- A hex address as the client writes it, Python's hex(): "0x" and digits. Lua's tonumber
--- takes no prefix with a base, so it is dropped first, as JavaScript's parseInt drops it.
local function hex(s)
    if not s then
        return nil
    end
    return tonumber((s:gsub("^0[xX]", "")), 16)
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
        local ty, addr, size = args[2], hex(args[3]), tonumber(args[4] or "", 10)
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
            local ty, size = item.type or "u8", item.size or 1
            if type(item.adr) ~= "number" or type(size) ~= "number" or size <= 0 or not known(ty) then
                return "Invalid Dict JSON Format"
            end
            keys[#keys + 1] = key
            reads[#reads + 1] = { type = ty, addr = math.tointeger(item.adr), size = math.tointeger(size) }
        end
        local values = read_all(reads)
        local out = {}
        for i, key in ipairs(keys) do
            out[key] = values[i]
        end
        return json.encode(out)
    elseif command == "write" then
        local ty, addr = args[2], hex(args[3])
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
            data = raw
        else
            local ok, values = pcall(json.decode, raw)
            if not ok or type(values) ~= "table" then
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
