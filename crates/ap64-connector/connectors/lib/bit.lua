-- BizHawk's `bit` library, on Lua 5.4's native operators, for connectors written against it.
-- BizHawk's is 32-bit C#; everything is masked so ~ and shifts behave the same.
--
-- Shift counts are masked to 5 bits, and that is not tidying. C# masks a 32-bit shift count
-- with 0x1F, so `>> 32` means `>> 0`; Lua 5.4 shifts 64-bit integers and yields 0.
-- connector_oot.lua's shop_check depends on the wrap: shop offsets run 0x1-0x8 and it
-- indexes `shop_offset*4 + item_offset` into one 32-bit word, so Market Potion Shop (0x8)
-- asks for bits 32-35, which wrap onto 0-3, where the game keeps that shop's purchases.
-- Unmasked, its four shopsanity slots never register (seen on hardware in oot-ap-cart).
local M = {}
local MASK = 0xFFFFFFFF

local function u32(v) return v & MASK end
local function count(n) return n & 31 end

function M.band(a, b)   return u32(a & b) end
function M.bor(a, b)    return u32(a | b) end
function M.bxor(a, b)   return u32(a ~ b) end
function M.bnot(a)      return u32(~a) end
function M.lshift(a, n) return u32(a << count(n)) end
function M.rshift(a, n) return u32(a) >> count(n) end               -- logical
function M.check(a, n)  return ((u32(a) >> count(n)) & 1) == 1 end  -- boolean
function M.set(a, n)    return u32(a | (1 << count(n))) end
function M.clear(a, n)  return u32(a & ~(1 << count(n))) end

return M
