--!nonstrict
-- Panel: status + record button, the last transcriptions (click to copy), sound settings.

local C = require("./common.luau")

local state = nil
local alive = true
local history = {}
local historyStamp = -1
local copiedAt = {} -- [index] = nowMs, for the brief "Copied" feedback
local volumeDraft = nil

local function statusText()
  if not alive or state == nil then
    return noctalia.tr("status.offline"), "on_surface_variant"
  end
  if state.enabled == false then
    return noctalia.tr("status.off"), "on_surface_variant"
  end
  if not state.ready then
    return noctalia.tr("status.loading"), "on_surface_variant"
  end
  if state.phase == "recording" then
    return noctalia.tr("status.listening"), "error"
  end
  if state.phase == "processing" then
    return noctalia.tr("status.processing"), "primary"
  end
  if state.last and C.sinceLast(state) < 4000 then
    local color = state.last == "error" and "error" or "on_surface_variant"
    return noctalia.tr("result." .. state.last), color
  end
  return noctalia.tr("status.ready"), "on_surface_variant"
end

local function card(children, key, grow)
  return ui.column({ key = key, flexGrow = grow, gap = 8, padding = 14, radius = 16, fill = "surface_variant/0.45", border = "outline/0.12", borderWidth = 1 }, children)
end

local function cardHeader(title, glyph, trailing)
  local row = {
    ui.glyph({ name = glyph, size = 15, color = "on_surface_variant" }),
    ui.label({ text = title, fontSize = 13, fontWeight = "semibold", color = "on_surface", flexGrow = 1 }),
  }
  if trailing then
    table.insert(row, trailing)
  end
  return ui.row({ gap = 8, align = "center", height = 26 }, row)
end

local function keycap(text)
  return ui.row({ paddingH = 7, height = 22, radius = 6, align = "center", fill = "surface/0.9", border = "outline/0.25", borderWidth = 1 }, {
    ui.label({ text = text, fontSize = 11, fontWeight = "medium", color = "on_surface" }),
  })
end

local function header()
  local text, color = statusText()
  local recording = state and state.phase == "recording"
  local busy = state and state.phase == "processing"
  local on = alive and state ~= nil and state.enabled ~= false
  local canRecord = on and state.ready and not busy
  local accent = recording and "error" or (on and "primary" or "on_surface_variant")
  return ui.row({ gap = 12, align = "center" }, {
    ui.row({ width = 44, height = 44, radius = 14, fill = accent .. "/0.14", align = "center", justify = "center" }, {
      ui.glyph({ name = on and "microphone" or "microphone-off", size = 22, color = accent }),
    }),
    ui.column({ gap = 1, flexGrow = 1, align = "start" }, {
      ui.label({ text = noctalia.tr("title"), fontSize = 17, fontWeight = "bold", color = "on_surface" }),
      ui.label({ text = text, fontSize = 12, color = color }),
    }),
    ui.column({ align = "center" }, {
      ui.toggle({
        checked = on,
        enabled = alive and state ~= nil,
        onChange = function(v)
          C.run({ "power", v == "true" and "on" or "off" })
        end,
      }),
    }),
    ui.button({
      glyph = recording and "player-stop-filled" or "microphone",
      glyphSize = 18,
      width = 44,
      height = 44,
      variant = recording and "destructive" or "primary",
      enabled = canRecord or recording,
      tooltip = recording and noctalia.tr("record.stop") or noctalia.tr("record.start"),
      onClick = function()
        if recording then
          C.run({ "toggle" })
        else
          -- Close first so the text field you came from gets focus back for typing.
          panel.close()
          C.run({ "toggle" })
        end
      end,
    }),
  })
end

local function historyRow(i, e)
  local justCopied = copiedAt[i] and noctalia.nowMs() - copiedAt[i] < 1500
  local meta = C.ago(e.at) .. " · " .. noctalia.tr(e.delivered == "typed" and "history.typed" or "history.clipboard")
  return ui.row({
    key = `h{i}-{e.at}`,
    gap = 10,
    paddingH = 10,
    paddingV = 8,
    radius = 10,
    align = "center",
    fill = justCopied and "primary/0.10" or "surface/0.55",
    onClick = function()
      noctalia.copyToClipboard(e.text, "text/plain;charset=utf-8")
      copiedAt[i] = noctalia.nowMs()
      render()
    end,
  }, {
    ui.column({ gap = 2, flexGrow = 1, align = "start" }, {
      ui.label({ text = e.text, fontSize = 13, color = "on_surface", maxLines = 2 }),
      ui.label({ text = meta, fontSize = 11, color = "on_surface_variant" }),
    }),
    ui.glyph({ name = justCopied and "check" or "copy", size = 14, color = justCopied and "primary" or "on_surface_variant" }),
  })
end

local function historyCard()
  local rows = {}
  if #history == 0 then
    table.insert(rows, ui.row({ justify = "center", paddingV = 18 }, {
      ui.label({ text = noctalia.tr("history.empty"), fontSize = 12, color = "on_surface_variant" }),
    }))
  else
    for i, e in history do
      table.insert(rows, historyRow(i, e))
    end
  end
  local clear = #history > 0 and ui.button({
    text = noctalia.tr("history.clear"), glyph = "trash", glyphSize = 13, controlSize = "sm", variant = "ghost",
    onClick = function()
      C.run({ "clear-history" }, function()
        history = {}
        render()
      end)
    end,
  }) or nil
  return card({
    cardHeader(noctalia.tr("history.title"), "history", clear),
    ui.scroll({ flexGrow = 1, gap = 6 }, rows),
  }, "history", 1)
end

local function soundCard()
  local on = state == nil or state.sound ~= false
  local vol = volumeDraft or (state and state.volume) or 0.35
  return card({
    cardHeader(noctalia.tr("sound.title"), on and "volume" or "volume-off", ui.toggle({
      checked = on,
      enabled = alive and state ~= nil,
      onChange = function(v)
        C.run({ "sound", v == "true" and "on" or "off" })
      end,
    })),
    ui.row({ gap = 12, align = "center", visible = on }, {
      ui.label({ text = noctalia.tr("sound.volume"), fontSize = 12, color = "on_surface_variant", width = 80 }),
      ui.slider({
        flexGrow = 1, min = 0, max = 1, step = 0.05, value = vol, controlSize = "sm",
        onChange = function(v)
          volumeDraft = tonumber(v)
        end,
        onDragEnd = function()
          if volumeDraft then
            C.run({ "volume", string.format("%.2f", volumeDraft) })
          end
        end,
      }),
    }),
  }, "sound")
end

local function footer()
  return ui.row({ gap = 6, align = "center", justify = "center" }, {
    keycap(noctalia.tr("key")),
    ui.label({ text = noctalia.tr("hint_hold") .. "  ·  " .. noctalia.tr("hint_cancel"), fontSize = 11, color = "on_surface_variant" }),
  })
end

function render()
  panel.render(ui.column({ flexGrow = 1, gap = 12, padding = 4 }, {
    header(),
    historyCard(),
    soundCard(),
    footer(),
  }))
end

local function refresh()
  state = C.readState()
  if state and volumeDraft and math.abs((state.volume or 0) - volumeDraft) < 0.01 then
    volumeDraft = nil
  end
  -- Reload history only when something new was delivered.
  local stamp = state and state.last_at or 0
  if stamp ~= historyStamp then
    historyStamp = stamp
    history = C.readHistory()
  end
  render()
end

function update()
  C.checkAlive(function(ok)
    alive = ok
  end)
  refresh()
end

function onOpen(_context)
  historyStamp = -1
  copiedAt = {}
  panel.setWantsSecondTicks(true)
  refresh()
end

function onClose()
  panel.setWantsSecondTicks(false)
end
