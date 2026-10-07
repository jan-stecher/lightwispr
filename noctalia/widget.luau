--!nonstrict
-- Bar widget: a calm mic glyph when idle, a live level meter while listening, a soft
-- pulse while transcribing, and a brief check / "copied" after delivery.
-- Left click: panel. Right click: start/stop a dictation.

local C = require("./common.luau")

local PANEL = "lightwispr/lightwispr:panel"
local FEEDBACK_MS = 1600
local ERROR_MS = 3500
-- Each bar reacts a little differently, so the meter looks like speech, not a VU block.
local BAR_WEIGHTS = { 0.7, 1.0, 0.85, 0.6 }
-- Every state renders at this width, so the rest of the bar never shifts.
local WIDTH = 30

local alive = true
local lastAliveCheck = 0
local bars = { 0, 0, 0, 0 }

local function frame(children, gap)
  return ui.row({ width = WIDTH, height = 18, align = "center", justify = "center", gap = gap or 0 }, children)
end

local function idleTree(color)
  return frame({ ui.glyph({ name = "microphone", size = 16, color = color }) })
end

local function meterTree(level)
  local t = noctalia.nowMs() / 1000
  local children = { ui.box({ width = 6, height = 6, radius = 3, fill = "error" }) }
  local gain = math.min(1, level * 1.8)
  for i, w in BAR_WEIGHTS do
    -- A little independent wobble per bar, scaled by the real input level.
    local wobble = 0.6 + 0.4 * math.sin(t * (7 + i * 1.7) + i * 1.3)
    local target = math.clamp(gain * w * wobble, 0, 1)
    bars[i] = bars[i] + (target - bars[i]) * 0.5
    local h = 4 + math.floor(bars[i] * 12 + 0.5)
    table.insert(children, ui.box({ width = 3, height = h, radius = 1.5, fill = "on_surface" }))
  end
  return frame(children, 2)
end

local function pulseTree()
  local t = noctalia.nowMs() / 1000
  return frame({
    ui.glyph({ name = "microphone", size = 16, color = "primary", opacity = 0.45 + 0.55 * (0.5 + 0.5 * math.sin(t * 5)) }),
  })
end

local function resultTree(glyph, color)
  return frame({ ui.glyph({ name = glyph, size = 16, color = color }) })
end

local function tooltip(state)
  if not alive or state == nil then
    return noctalia.tr("status.offline")
  end
  if not state.ready then
    return noctalia.tr("status.loading")
  end
  return { { key = noctalia.tr("title"), value = noctalia.tr("hint") } }
end

local function render()
  local state = alive and C.readState() or nil
  local fast = false
  local tree

  if state == nil or not alive then
    tree = frame({ ui.glyph({ name = "microphone-off", size = 16, color = "on_surface_variant" }) })
  elseif state.phase == "recording" then
    tree = meterTree(state.level or 0)
    fast = true
  elseif state.phase == "processing" then
    tree = pulseTree()
    fast = true
  else
    local since = C.sinceLast(state)
    if state.last == "typed" and since < FEEDBACK_MS then
      tree = resultTree("check", "primary")
      fast = true
    elseif state.last == "clipboard" and since < FEEDBACK_MS then
      tree = resultTree("clipboard-check", "primary")
      fast = true
    elseif state.last == "error" and since < ERROR_MS then
      tree = resultTree("alert-triangle", "error")
      fast = true
    else
      tree = idleTree(state.ready and "on_surface" or "on_surface_variant")
    end
  end

  barWidget.render(tree)
  barWidget.setTooltip(tooltip(state))
  -- Smooth while something moves, near-free when idle.
  noctalia.setUpdateInterval(fast and 40 or 300)
end

function update()
  local now = noctalia.nowMs()
  if now - lastAliveCheck > 4000 then
    lastAliveCheck = now
    C.checkAlive(function(ok)
      alive = ok
    end)
  end
  render()
end

function onClick()
  noctalia.togglePanel(PANEL)
end

function onRightClick()
  C.run({ "toggle" })
end
