#!lua name=antirion_v1

-- Counter arithmetic mirrored from the Rust model (antirion_limits::math). Lua 5.1 numbers are
-- doubles, exact only below 2^53, so every amount is an exact signed integer N: little-endian limbs
-- in base 2^24, where a limb product stays below 2^48 and every intermediate step is exact.
-- Doubles appear only in TTLs, never in a decision or a stored value.

local BASE = 16777216

local function trim(a)
  local n = #a
  while n > 0 and a[n] == 0 do
    a[n] = nil
    n = n - 1
  end
  if n == 0 then a.neg = false end
  return a
end

local function nint(x)
  local a = { neg = x < 0 }
  x = math.abs(x)
  local i = 1
  while x > 0 do
    local r = x % BASE
    a[i] = r
    x = (x - r) / BASE
    i = i + 1
  end
  return trim(a)
end

-- Load time has no standard library, so constants are written as limbs.
local ZERO = { neg = false }
local MILLION = { 1000000, neg = false }

local function cmpmag(a, b)
  if #a ~= #b then return (#a < #b) and -1 or 1 end
  for i = #a, 1, -1 do
    if a[i] ~= b[i] then return (a[i] < b[i]) and -1 or 1 end
  end
  return 0
end

local function addmag(a, b)
  local r, c = { neg = false }, 0
  for i = 1, math.max(#a, #b) do
    local s = (a[i] or 0) + (b[i] or 0) + c
    if s >= BASE then r[i] = s - BASE; c = 1 else r[i] = s; c = 0 end
  end
  if c > 0 then r[#r + 1] = c end
  return r
end

local function submag(a, b)
  local r, c = { neg = false }, 0
  for i = 1, #a do
    local s = a[i] - (b[i] or 0) - c
    if s < 0 then r[i] = s + BASE; c = 1 else r[i] = s; c = 0 end
  end
  return trim(r)
end

local function negate(a)
  local r = { neg = false }
  for i = 1, #a do r[i] = a[i] end
  r.neg = (#r > 0) and not a.neg
  return r
end

local function add(a, b)
  if a.neg == b.neg then
    local r = addmag(a, b)
    r.neg = a.neg and #r > 0
    return r
  end
  local c = cmpmag(a, b)
  if c == 0 then return nint(0) end
  local r
  if c > 0 then r = submag(a, b); r.neg = a.neg else r = submag(b, a); r.neg = b.neg end
  return r
end

local function sub(a, b) return add(a, negate(b)) end

local function cmp(a, b)
  if a.neg ~= b.neg then return a.neg and -1 or 1 end
  local c = cmpmag(a, b)
  if a.neg then return -c end
  return c
end

local function mul(a, b)
  local r = { neg = false }
  for i = 1, #a + #b do r[i] = 0 end
  for i = 1, #a do
    local c, ai = 0, a[i]
    for j = 1, #b do
      local t = r[i + j - 1] + ai * b[j] + c
      c = math.floor(t / BASE)
      r[i + j - 1] = t - c * BASE
    end
    local k = i + #b
    while c > 0 do
      local t = r[k] + c
      c = math.floor(t / BASE)
      r[k] = t - c * BASE
      k = k + 1
    end
  end
  r.neg = a.neg ~= b.neg
  return trim(r)
end

local function bitlen(a)
  if #a == 0 then return 0 end
  local top, n = a[#a], (#a - 1) * 24
  while top > 0 do
    n = n + 1
    top = math.floor(top / 2)
  end
  return n
end

local function bit(a, i)
  local limb = a[math.floor(i / 24) + 1] or 0
  return math.floor(limb / 2 ^ (i % 24)) % 2
end

local function shl1(a, b)
  local r, c = { neg = false }, b
  for i = 1, #a do
    local s = a[i] * 2 + c
    if s >= BASE then r[i] = s - BASE; c = 1 else r[i] = s; c = 0 end
  end
  if c > 0 then r[#r + 1] = c end
  return r
end

-- Quotient and remainder of magnitudes, b > 0.
local function divmod(a, b)
  local q, r = { neg = false }, { neg = false }
  for i = bitlen(a) - 1, 0, -1 do
    r = shl1(r, bit(a, i))
    if cmpmag(r, b) >= 0 then
      r = submag(r, b)
      q = shl1(q, 1)
    else
      q = shl1(q, 0)
    end
  end
  return trim(q), trim(r)
end

local function ceil_div(a, b)
  local q, r = divmod(a, b)
  if #r > 0 then q = add(q, nint(1)) end
  return q
end

local function floor_div(a, b)
  local q = divmod(a, b)
  return q
end

local function parse(s)
  s = tostring(s or '0')
  local neg = false
  if string.sub(s, 1, 1) == '-' then neg = true; s = string.sub(s, 2) end
  local a, len = nint(0), #s
  local pos, chunk = 1, len % 6
  if chunk == 0 then chunk = 6 end
  while pos <= len do
    a = add(mul(a, MILLION), nint(tonumber(string.sub(s, pos, pos + chunk - 1))))
    pos = pos + chunk
    chunk = 6
  end
  a.neg = neg and #a > 0
  return a
end

local function str(a)
  if #a == 0 then return '0' end
  local cur, parts = { neg = false }, {}
  for i = 1, #a do cur[i] = a[i] end
  while #cur > 0 do
    local rem = 0
    for i = #cur, 1, -1 do
      local t = rem * BASE + cur[i]
      local q = math.floor(t / 1000000)
      rem = t - q * 1000000
      cur[i] = q
    end
    trim(cur)
    table.insert(parts, 1, (#cur > 0) and string.format('%06d', rem) or string.format('%d', rem))
  end
  local s = table.concat(parts)
  if a.neg then s = '-' .. s end
  return s
end

local function min(a, b) if cmp(a, b) <= 0 then return a end return b end
local function max0(a) if a.neg then return nint(0) end return a end
local function num(a) return tonumber(str(a)) end

-- GCRA: `a` is available units × T; refill adds L per microsecond up to B·T.
local function gcra_refilled(L, T, B, st, now)
  local cap = mul(B, T)
  if not st then return cap end
  local a = st.a
  if st.p and #st.p > 0 and cmp(st.p, T) ~= 0 then
    local q = floor_div(mul(a, T), st.p)
    if a.neg then q = negate(q) end
    a = q
  end
  a = add(a, mul(sub(B, st.b), T))
  local dt = max0(sub(now, st.t))
  return min(add(a, mul(dt, L)), cap)
end

local function gcra_admit(L, T, B, st, now, rho)
  if cmp(rho, B) > 0 then return 'exceeds' end
  local a = gcra_refilled(L, T, B, st, now)
  local need = mul(rho, T)
  if cmp(need, a) <= 0 then return 'admit', { a = sub(a, need), t = now, b = B, p = T } end
  if #L == 0 then return 'wait', nil end
  return 'wait', ceil_div(sub(need, a), L)
end

local function gcra_settle(L, T, B, st, now, rho, actual)
  local a = gcra_refilled(L, T, B, st, now)
  if cmp(actual, rho) <= 0 then
    a = min(add(a, mul(sub(rho, actual), T)), mul(B, T))
  else
    a = sub(a, mul(sub(actual, rho), T))
  end
  return { a = a, t = now, b = B, p = T }
end

local function gcra_view(L, T, B, st, now)
  local a = gcra_refilled(L, T, B, st, now)
  local remaining = floor_div(max0(a), T)
  local missing = sub(mul(B, T), a)
  if missing.neg or #missing == 0 or #L == 0 then return remaining, ZERO end
  return remaining, ceil_div(missing, L)
end

-- Sliding window: charges by sub-window k = floor(now·P/T), expiring at ceil(((k+1)·T + T·P)/P).
local function win_slot(T, P, now) return floor_div(mul(now, P), T) end

local function win_expiry(T, P, k)
  return ceil_div(add(mul(add(k, nint(1)), T), mul(T, P)), P)
end

local function win_load(key)
  local raw = redis.call('HGETALL', key)
  local slots = {}
  for i = 1, #raw, 2 do slots[#slots + 1] = { k = parse(raw[i]), v = parse(raw[i + 1]) } end
  table.sort(slots, function(x, y) return cmp(x.k, y.k) < 0 end)
  return slots
end

local function win_prune(T, P, slots, now)
  local live = {}
  for _, s in ipairs(slots) do
    if #s.v > 0 and not s.v.neg and cmp(now, win_expiry(T, P, s.k)) < 0 then live[#live + 1] = s end
  end
  return live
end

local function win_sum(slots)
  local sum = nint(0)
  for _, s in ipairs(slots) do sum = add(sum, s.v) end
  return sum
end

local function win_charge(slots, k, delta)
  for _, s in ipairs(slots) do
    if cmp(s.k, k) == 0 then
      s.v = max0(add(s.v, delta))
      return slots
    end
  end
  slots[#slots + 1] = { k = k, v = max0(delta) }
  table.sort(slots, function(x, y) return cmp(x.k, y.k) < 0 end)
  return slots
end

local function win_admit(L, T, P, slots, now, rho)
  if cmp(rho, L) > 0 then return 'exceeds' end
  slots = win_prune(T, P, slots, now)
  local sum = win_sum(slots)
  if cmp(add(sum, rho), L) <= 0 then
    local k = win_slot(T, P, now)
    return 'admit', win_charge(slots, k, rho), k
  end
  local left = sum
  for _, s in ipairs(slots) do
    left = sub(left, s.v)
    if cmp(add(left, rho), L) <= 0 then return 'wait', sub(win_expiry(T, P, s.k), now) end
  end
  return 'wait', T
end

local function win_view(L, T, P, slots, now)
  local live = win_prune(T, P, slots, now)
  local remaining = max0(sub(L, win_sum(live)))
  if #live == 0 then return remaining, ZERO end
  return remaining, max0(sub(win_expiry(T, P, live[#live].k), now))
end

local function now_us()
  local t = redis.call('TIME')
  return nint(tonumber(t[1]) * 1000000 + tonumber(t[2]))
end

-- A hold: algo (G, W, C, B), mode (E enforce, S shadow, T track), p1..p3, rho, and the sub-window.
local function hold_at(argv, base, i)
  local o = base + (i - 1) * 6
  return { algo = argv[o + 1], mode = argv[o + 2], p1 = argv[o + 3], p2 = argv[o + 4], p3 = argv[o + 5],
           rho = argv[o + 6], slot = '0' }
end

local function hold_str(h)
  return table.concat({ h.algo, h.mode, h.p1, h.p2, h.p3, h.rho, h.slot }, ',')
end

local function fields(s, sep)
  local out, start = {}, 1
  while true do
    local i = string.find(s, sep, start, true)
    if not i then
      out[#out + 1] = string.sub(s, start)
      return out
    end
    out[#out + 1] = string.sub(s, start, i - 1)
    start = i + 1
  end
end

local function split(s, sep)
  if s == nil or s == '' then return {} end
  return fields(s, sep)
end

local function hold_parse(s)
  local f = fields(s, ',')
  return { algo = f[1], mode = f[2], p1 = f[3], p2 = f[4], p3 = f[5], rho = f[6], slot = f[7] }
end

local function gcra_load(key)
  local cur = redis.call('HMGET', key, 'a', 't', 'b', 'p')
  if not cur[1] then return nil end
  return { a = parse(cur[1]), t = parse(cur[2]), b = parse(cur[3]), p = cur[4] and parse(cur[4]) or nil }
end

local function gcra_store(key, L, T, B, st)
  redis.call('HSET', key, 'a', str(st.a), 't', str(st.t), 'b', str(st.b), 'p', str(T))
  if #L == 0 then
    redis.call('PERSIST', key)
  else
    local missing = max0(sub(mul(B, T), st.a))
    local idle_ms = math.floor(num(ceil_div(missing, L)) / 1000) + 1000
    redis.call('PEXPIRE', key, math.min(idle_ms, 4503599627370496))
  end
end

local function win_store(key, T, P, slots)
  redis.call('DEL', key)
  for _, s in ipairs(slots) do
    if #s.v > 0 then redis.call('HSET', key, str(s.k), str(s.v)) end
  end
  local ttl = math.floor(num(add(T, ceil_div(T, P))) / 1000) + 1000
  if #slots > 0 then redis.call('PEXPIRE', key, ttl) end
end

local function budget_load(key)
  local cur = redis.call('HMGET', key, 's', 'r')
  return parse(cur[1] or '0'), parse(cur[2] or '0')
end

local function budget_store(key, s, r, ending)
  redis.call('HSET', key, 's', str(s), 'r', str(r))
  if ending ~= '' then
    redis.call('PEXPIREAT', key, math.floor(tonumber(ending) / 1000) + 604800000)
  end
end

-- The view of one counter: remaining, microseconds to full, spent, reserved.
local function view(key, h, now)
  if h.algo == 'G' then
    local r, f = gcra_view(parse(h.p1), parse(h.p2), parse(h.p3), gcra_load(key), now)
    return { str(r), str(f), '0', '0' }
  elseif h.algo == 'W' then
    local r, f = win_view(parse(h.p1), parse(h.p2), parse(h.p3), win_load(key), now)
    return { str(r), str(f), '0', '0' }
  elseif h.algo == 'R' then
    local T, P = parse(h.p2), parse(h.p3)
    local slots = win_load(key)
    local r, f = win_view(parse(h.p1), T, P, slots, now)
    return { str(r), str(f), str(win_sum(win_prune(T, P, slots, now))), '0' }
  elseif h.algo == 'C' then
    local g = parse(redis.call('HGET', key, 'g') or '0')
    return { str(max0(sub(parse(h.p1), g))), '0', '0', '0' }
  end
  local s, r = budget_load(key)
  return { str(max0(sub(parse(h.p1), add(s, r)))), '0', str(s), str(r) }
end

-- What the headroom holds right after hold i have left: an enforced hold keeps it free.
local function headroom(keys, kbase, holds, i, now)
  local extra, j = nint(0), i + 1
  while holds[j] and holds[j].mode == 'H' do
    extra = add(extra, parse(view(keys[kbase + j], holds[j], now)[1]))
    j = j + 1
  end
  return extra
end

-- Checks every hold against the current state; returns the writes, or the refusals.
local function take(keys, kbase, holds, now)
  local writes, refused = {}, {}
  for i, h in ipairs(holds) do
    local key, rho, enforce = keys[kbase + i], parse(h.rho), h.mode == 'E'
    local w = nil
    local extra = nint(0)
    if enforce then extra = headroom(keys, kbase, holds, i, now) end
    if h.mode == 'T' or h.mode == 'H' then
      w = nil
    elseif h.algo == 'G' then
      local L, T, B = parse(h.p1), parse(h.p2), parse(h.p3)
      local st = gcra_load(key)
      if enforce then
        local verdict, x = gcra_admit(L, T, B, st, now, rho)
        if verdict == 'admit' and #extra > 0 then
          local v2, x2 = gcra_admit(L, T, B, st, now, add(rho, extra))
          if v2 == 'wait' then verdict, x = 'wait', x2 elseif v2 == 'exceeds' then verdict, x = 'wait', nil end
        end
        if verdict == 'admit' then w = function() gcra_store(key, L, T, B, x) end
        elseif verdict == 'exceeds' then refused[#refused + 1] = { tostring(i), 'exceeds', '' }
        else refused[#refused + 1] = { tostring(i), 'rate', x and str(x) or '' } end
      else
        local x = gcra_settle(L, T, B, st, now, ZERO, rho)
        w = function() gcra_store(key, L, T, B, x) end
      end
    elseif h.algo == 'W' then
      local L, T, P = parse(h.p1), parse(h.p2), parse(h.p3)
      local slots = win_load(key)
      if enforce then
        local verdict, x, k = win_admit(L, T, P, slots, now, rho)
        if verdict == 'admit' and #extra > 0 then
          local v2, x2 = win_admit(L, T, P, win_load(key), now, add(rho, extra))
          if v2 == 'wait' then verdict, x = 'wait', x2 elseif v2 == 'exceeds' then verdict, x = 'wait', nil end
        end
        if verdict == 'admit' then
          h.slot = str(k)
          w = function() win_store(key, T, P, x) end
        elseif verdict == 'exceeds' then refused[#refused + 1] = { tostring(i), 'exceeds', '' }
        else refused[#refused + 1] = { tostring(i), 'rate', x and str(x) or '' } end
      else
        local k = win_slot(T, P, now)
        local x = win_charge(win_prune(T, P, slots, now), k, rho)
        h.slot = str(k)
        w = function() win_store(key, T, P, x) end
      end
    elseif h.algo == 'R' then
      local L, T, P = parse(h.p1), parse(h.p2), parse(h.p3)
      local slots = win_load(key)
      if enforce then
        local verdict, x, k = win_admit(L, T, P, slots, now, rho)
        if verdict == 'admit' then
          h.slot = str(k)
          w = function() win_store(key, T, P, x) end
        elseif verdict == 'exceeds' then refused[#refused + 1] = { tostring(i), 'budget', '' }
        else refused[#refused + 1] = { tostring(i), 'budget', str(x) } end
      else
        local k = win_slot(T, P, now)
        local x = win_charge(win_prune(T, P, slots, now), k, rho)
        h.slot = str(k)
        w = function() win_store(key, T, P, x) end
      end
    elseif h.algo == 'C' then
      local L = parse(h.p1)
      local g = parse(redis.call('HGET', key, 'g') or '0')
      local next_g = add(g, rho)
      if enforce and #L == 0 then refused[#refused + 1] = { tostring(i), 'exceeds', '' }
      elseif enforce and cmp(add(next_g, extra), L) > 0 then refused[#refused + 1] = { tostring(i), 'rate', '1000000' }
      else w = function() redis.call('HSET', key, 'g', str(next_g)) end end
    else
      local s, r = budget_load(key)
      local amount = parse(h.p1)
      if enforce and cmp(add(add(s, r), rho), amount) > 0 then
        refused[#refused + 1] = { tostring(i), 'budget', '' }
      else
        local r1 = add(r, rho)
        w = function() budget_store(key, s, r1, h.p2) end
      end
    end
    writes[i] = w
  end
  return writes, refused
end

local function apply(writes, n)
  for i = 1, n do
    if writes[i] then writes[i]() end
  end
end

local function views(keys, kbase, holds, now)
  local out = {}
  for i, h in ipairs(holds) do out[i] = view(keys[kbase + i], h, now) end
  return out
end

local function read_holds(argv, base)
  local n, holds = tonumber(argv[base]), {}
  for i = 1, n do holds[i] = hold_at(argv, base, i) end
  return holds
end

-- KEYS: reservation, node set, nodes, then one counter key per hold.
-- ARGV: node, n, then six fields per hold.
local function admit(keys, argv)
  local st = redis.call('HGET', keys[1], 'st')
  if st then return { 'replay', st } end
  local now = now_us()
  local holds = read_holds(argv, 2)
  local writes, refused = take(keys, 3, holds, now)
  if #refused > 0 then return { 'refused', refused } end
  apply(writes, #holds)
  local hs, ks = {}, {}
  for i, h in ipairs(holds) do
    hs[i] = hold_str(h)
    ks[i] = keys[3 + i]
  end
  redis.call('HSET', keys[1], 'st', 'held', 'n', argv[1], 'h', table.concat(hs, ';'),
             'k', table.concat(ks, ';'), 'a', ',', 'at', str(now))
  redis.call('SADD', keys[2], keys[1])
  redis.call('SADD', keys[3], argv[1])
  return { 'admitted', views(keys, 3, holds, now) }
end

-- KEYS: reservation, then one counter key per hold. ARGV: attempt, n, six fields per hold.
local function admit_attempt(keys, argv)
  local rec = redis.call('HMGET', keys[1], 'st', 'a', 'h', 'k')
  if not rec[1] then return { 'replay', 'cancelled' } end
  local tag = ',' .. argv[1] .. ','
  if rec[1] ~= 'held' then return { 'replay', rec[1] } end
  if string.find(rec[2] or ',', tag, 1, true) then return { 'replay', 'held' } end
  local now = now_us()
  local holds = read_holds(argv, 2)
  local writes, refused = take(keys, 1, holds, now)
  if #refused > 0 then return { 'refused', refused } end
  apply(writes, #holds)
  local hs, ks = { rec[3] }, { rec[4] }
  for i, h in ipairs(holds) do
    hs[#hs + 1] = hold_str(h)
    ks[#ks + 1] = keys[1 + i]
  end
  redis.call('HSET', keys[1], 'a', (rec[2] or ',') .. argv[1] .. ',', 'h', table.concat(hs, ';'),
             'k', table.concat(ks, ';'))
  return { 'admitted', views(keys, 1, holds, now) }
end

-- Replaces each hold by its actual amount; budget changes as {index, before, after}.
local function settle(keys, kbase, held, actual, now)
  local changes = {}
  for i, h in ipairs(held) do
    local key, rho, a = keys[kbase + i], parse(h.rho), actual[i]
    if h.mode == 'H' then
      -- never charged: nothing to settle
    elseif h.algo == 'G' then
      local L, T, B = parse(h.p1), parse(h.p2), parse(h.p3)
      gcra_store(key, L, T, B, gcra_settle(L, T, B, gcra_load(key), now, rho, a))
    elseif h.algo == 'W' then
      local T, P = parse(h.p2), parse(h.p3)
      local slots = win_charge(win_load(key), parse(h.slot), sub(a, rho))
      win_store(key, T, P, win_prune(T, P, slots, now))
    elseif h.algo == 'R' then
      local T, P = parse(h.p2), parse(h.p3)
      local slots = win_prune(T, P, win_load(key), now)
      local before = win_sum(slots)
      local k, delta = parse(h.slot), sub(a, rho)
      if h.mode == 'T' then k, delta = win_slot(T, P, now), a end
      slots = win_prune(T, P, win_charge(slots, k, delta), now)
      win_store(key, T, P, slots)
      changes[#changes + 1] = { tostring(i), str(before), str(win_sum(slots)) }
    elseif h.algo == 'C' then
      local g = max0(sub(parse(redis.call('HGET', key, 'g') or '0'), rho))
      redis.call('HSET', key, 'g', str(g))
    else
      local s, r = budget_load(key)
      local held_rho = rho
      if h.mode == 'T' then held_rho = nint(0) end
      local s1 = add(s, a)
      budget_store(key, s1, max0(sub(r, held_rho)), h.p2)
      changes[#changes + 1] = { tostring(i), str(s), str(s1) }
    end
  end
  return changes
end

local function held_of(rec_h)
  local held = {}
  for _, s in ipairs(split(rec_h, ';')) do held[#held + 1] = hold_parse(s) end
  return held
end

-- KEYS: reservation, node set, then every counter key in hold order. ARGV: keep_ms, n, actuals.
local function reconcile(keys, argv)
  local rec = redis.call('HMGET', keys[1], 'st', 'h')
  if not rec[1] then
    redis.call('HSET', keys[1], 'st', 'cancelled')
    redis.call('PEXPIRE', keys[1], tonumber(argv[1]))
    return { 'noop', '' }
  end
  if rec[1] ~= 'held' then return { 'noop', rec[1] } end
  local held = held_of(rec[2])
  local n = tonumber(argv[2])
  if n ~= #held or #keys ~= 2 + n then return redis.error_reply('ERR hold count mismatch') end
  local actual = {}
  for i = 1, n do actual[i] = parse(argv[2 + i]) end
  local changes = settle(keys, 2, held, actual, now_us())
  redis.call('HSET', keys[1], 'st', 'settled')
  redis.call('PEXPIRE', keys[1], tonumber(argv[1]))
  redis.call('SREM', keys[2], keys[1])
  return { 'settled', changes }
end

-- KEYS: reservation, node set, then the counter keys of its record. ARGV: keep_ms.
-- Concurrency is released; every other hold stays charged at its reservation.
local function expire(keys, argv)
  local rec = redis.call('HMGET', keys[1], 'st', 'h', 'n')
  if rec[1] ~= 'held' then
    redis.call('SREM', keys[2], keys[1])
    return { 'noop', rec[1] or '' }
  end
  local held = held_of(rec[2])
  if #keys ~= 2 + #held then return redis.error_reply('ERR hold count mismatch') end
  local actual = {}
  for i, h in ipairs(held) do actual[i] = parse(h.rho) end
  settle(keys, 2, held, actual, now_us())
  redis.call('HSET', keys[1], 'st', 'expired')
  redis.call('PEXPIRE', keys[1], tonumber(argv[1]))
  redis.call('SREM', keys[2], keys[1])
  return { 'expired', rec[3] or '', rec[2] }
end

-- KEYS: counter keys. ARGV: n, six fields per hold.
local function read(keys, argv)
  return views(keys, 0, read_holds(argv, 1), now_us())
end

-- KEYS: budget key. ARGV: spent, period end (or ''). Raises the spend, never lowers it.
local function seed(keys, argv)
  local s, r = budget_load(keys[1])
  local seeded = parse(argv[1])
  if cmp(seeded, s) > 0 then budget_store(keys[1], seeded, r, argv[2]) end
  return 'ok'
end

-- KEYS: budget key. ARGV: signed delta, period end (or ''). Refuses a negative result.
local function adjust(keys, argv)
  local s, r = budget_load(keys[1])
  local s1 = add(s, parse(argv[1]))
  if s1.neg then return { 'negative' } end
  budget_store(keys[1], s1, r, argv[2])
  return { 'ok', str(s1) }
end

-- KEYS: rolling budget key. ARGV: T, P, then sub-window and charge pairs. Raises each sub-window
-- to at least its charge, never lowers one.
local function seed_window(keys, argv)
  local T, P = parse(argv[1]), parse(argv[2])
  local slots = win_load(keys[1])
  for i = 3, #argv - 1, 2 do
    local k, v = parse(argv[i]), parse(argv[i + 1])
    local found = false
    for _, s in ipairs(slots) do
      if cmp(s.k, k) == 0 then
        if cmp(v, s.v) > 0 then s.v = v end
        found = true
      end
    end
    if not found then slots[#slots + 1] = { k = k, v = v } end
  end
  table.sort(slots, function(x, y) return cmp(x.k, y.k) < 0 end)
  win_store(keys[1], T, P, win_prune(T, P, slots, now_us()))
  return 'ok'
end

-- KEYS: rolling budget key. ARGV: signed delta, T, P. A charge goes to the current sub-window, a
-- credit comes off the oldest ones first; refuses a credit larger than the charges.
local function adjust_window(keys, argv)
  local delta, T, P = parse(argv[1]), parse(argv[2]), parse(argv[3])
  local now = now_us()
  local slots = win_prune(T, P, win_load(keys[1]), now)
  if not delta.neg then
    slots = win_charge(slots, win_slot(T, P, now), delta)
  else
    local credit = negate(delta)
    if cmp(credit, win_sum(slots)) > 0 then return { 'negative' } end
    for _, s in ipairs(slots) do
      local take = min(credit, s.v)
      s.v = sub(s.v, take)
      credit = sub(credit, take)
    end
    slots = win_prune(T, P, slots, now)
  end
  win_store(keys[1], T, P, slots)
  return { 'ok', str(win_sum(slots)) }
end

-- Pure arithmetic with an explicit clock, for the differential test against the Rust model.
local function eval(_, argv)
  local op = argv[1]
  local function st_of(a, t, b, p)
    if a == '' then return nil end
    return { a = parse(a), t = parse(t), b = parse(b), p = parse(p) }
  end
  if op == 'gcra_admit' then
    local verdict, x = gcra_admit(parse(argv[2]), parse(argv[3]), parse(argv[4]),
                                  st_of(argv[5], argv[6], argv[7], argv[8]), parse(argv[9]), parse(argv[10]))
    if verdict == 'admit' then return { 'admit', str(x.a), str(x.t), str(x.b), str(x.p) } end
    if verdict == 'wait' then return { 'wait', x and str(x) or '' } end
    return { 'exceeds' }
  elseif op == 'gcra_settle' then
    local x = gcra_settle(parse(argv[2]), parse(argv[3]), parse(argv[4]),
                          st_of(argv[5], argv[6], argv[7], argv[8]), parse(argv[9]), parse(argv[10]), parse(argv[11]))
    return { str(x.a), str(x.t), str(x.b), str(x.p) }
  elseif op == 'gcra_view' then
    local r, f = gcra_view(parse(argv[2]), parse(argv[3]), parse(argv[4]),
                           st_of(argv[5], argv[6], argv[7], argv[8]), parse(argv[9]))
    return { str(r), str(f) }
  elseif op == 'window_admit' then
    local slots = {}
    for _, kv in ipairs(split(argv[5], ';')) do
      local parts = split(kv, ':')
      slots[#slots + 1] = { k = parse(parts[1]), v = parse(parts[2]) }
    end
    table.sort(slots, function(x, y) return cmp(x.k, y.k) < 0 end)
    local verdict, x, k = win_admit(parse(argv[2]), parse(argv[3]), parse(argv[4]), slots, parse(argv[6]), parse(argv[7]))
    if verdict == 'admit' then
      local out = {}
      for _, s in ipairs(x) do out[#out + 1] = str(s.k) .. ':' .. str(s.v) end
      return { 'admit', table.concat(out, ';'), str(k) }
    end
    if verdict == 'wait' then return { 'wait', str(x) } end
    return { 'exceeds' }
  elseif op == 'arith' then
    local a, b = parse(argv[2]), parse(argv[3])
    local q, r = divmod(max0(a), b)
    return { str(add(a, b)), str(sub(a, b)), str(mul(a, b)), str(q), str(r), tostring(cmp(a, b)) }
  end
  return redis.error_reply('ERR unknown op')
end

redis.register_function('antirion_admit', admit)
redis.register_function('antirion_admit_attempt', admit_attempt)
redis.register_function('antirion_reconcile', reconcile)
redis.register_function('antirion_expire', expire)
redis.register_function('antirion_seed', seed)
redis.register_function('antirion_adjust', adjust)
redis.register_function('antirion_seed_window', seed_window)
redis.register_function('antirion_adjust_window', adjust_window)
redis.register_function { function_name = 'antirion_read', callback = read, flags = { 'no-writes' } }
redis.register_function { function_name = 'antirion_eval', callback = eval, flags = { 'no-writes' } }
