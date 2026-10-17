--!nonstrict
-- Panel: status + record button, the last transcriptions (click to copy), processing
-- (speech engine, enhancement style, text-field context) and settings.

local C = require("./common.luau")

local state = nil
local alive = true
local history = {}
local historyStamp = -1
local copiedAt = {} -- [index] = nowMs, for the brief "Copied" feedback
local volumeDraft = nil
-- Same order as the daemon's presets (src/hotkey.rs).
local HOTKEYS = { "right_ctrl", "ctrl_super", "super_alt" }
local ENGINES = { "parakeet", "whisper" }

local function hotkeyId()
  return (state and state.hotkey) or "right_ctrl"
end

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
  if state.phase == "enhancing" then
    return noctalia.tr("status.enhancing"), "primary"
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
  local busy = state and (state.phase == "processing" or state.phase == "enhancing")
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
  local meta = C.ago(e.at) .. " · " .. noctalia.tr((e.delivered == "typed" or e.delivered == "pasted") and "history.typed" or "history.clipboard")
  if e.mode then
    meta = meta .. " · " .. C.modeName(state, e.mode)
  end
  local children = {
    ui.column({ gap = 2, flexGrow = 1, align = "start" }, {
      ui.label({ text = e.text, fontSize = 13, color = "on_surface", maxLines = 2 }),
      ui.label({ text = meta, fontSize = 11, color = "on_surface_variant" }),
    }),
  }
  if e.raw then
    table.insert(children, ui.button({
      glyph = "arrow-back-up", glyphSize = 13, controlSize = "sm", variant = "ghost", tooltip = noctalia.tr("history.original"),
      onClick = function()
        noctalia.copyToClipboard(e.raw, "text/plain;charset=utf-8")
        copiedAt[i] = noctalia.nowMs()
        render()
      end,
    }))
  end
  table.insert(children, ui.glyph({ name = justCopied and "check" or "copy", size = 14, color = justCopied and "primary" or "on_surface_variant" }))
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
  }, children)
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

local function settingRow(label, control, labelWidth)
  return ui.row({ gap = 12, align = "center", height = 32 }, {
    ui.label({ text = label, fontSize = 12, color = "on_surface_variant", width = labelWidth or 92 }),
    ui.row({ flexGrow = 1, align = "center", justify = "end" }, { control }),
  })
end

local function settingsCard()
  local ready = alive and state ~= nil
  local on = state == nil or state.sound ~= false
  local vol = volumeDraft or (state and state.volume) or 0.35
  local options, selected = {}, 0
  for i, id in HOTKEYS do
    table.insert(options, noctalia.tr("hotkey." .. id))
    if id == hotkeyId() then
      selected = i - 1
    end
  end
  return card({
    cardHeader(noctalia.tr("settings"), "adjustments-horizontal", nil),
    settingRow(noctalia.tr("hotkey.label"), ui.select({
      width = 180, controlSize = "sm", options = options, selectedIndex = selected, enabled = ready,
      onChange = function(index)
        local id = HOTKEYS[(tonumber(index) or 0) + 1]
        if id then
          C.run({ "hotkey", id })
        end
      end,
    })),
    settingRow(noctalia.tr("sound.enabled"), ui.toggle({
      checked = on,
      enabled = ready,
      onChange = function(v)
        C.run({ "sound", v == "true" and "on" or "off" })
      end,
    })),
    ui.row({ gap = 12, align = "center", height = 28, visible = on }, {
      ui.label({ text = noctalia.tr("sound.volume"), fontSize = 12, color = "on_surface_variant", width = 92 }),
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
  }, "settings")
end

-- The daemon's catalog says which models are on disk; an older daemon has none.
local function installed(key)
  local c = state and state.catalog
  return c == nil or c.installed == nil or c.installed[key] ~= false
end

local function sizeMb(key)
  local c = state and state.catalog
  return (c and c.sizes and c.sizes[key]) or 0
end

local function downloadButton(key)
  return ui.button({
    text = noctalia.tr("proc.download", { mb = sizeMb(key) }), glyph = "download", glyphSize = 13, controlSize = "sm", variant = "outline",
    enabled = alive and state ~= nil and state.download == nil,
    onClick = function()
      C.run({ "download", key })
    end,
  })
end

local function editPrompt(mode)
  noctalia.runAsync({ C.bin, "mode-file", mode }, function(r)
    if r and r.exitCode == 0 then
      local path = string.gsub(r.stdout, "%s+$", "")
      if path ~= "" then
        noctalia.runAsync({ "xdg-open", path })
      end
    end
  end, 3000)
  panel.close()
end

local function processingCard()
  local ready = alive and state ~= nil and state.catalog ~= nil
  local engine = (state and state.stt) or "parakeet"
  local mode = C.mode(state)

  local options, selected = {}, 0
  for i, id in ENGINES do
    local label = noctalia.tr("engine." .. id)
    if not installed(id) then
      label = label .. " · " .. noctalia.tr("proc.not_loaded")
    end
    table.insert(options, label)
    if id == engine then
      selected = i - 1
    end
  end
  local rows = {
    cardHeader(noctalia.tr("proc.title"), "sparkles", nil),
    settingRow(noctalia.tr("proc.engine"), ui.select({
      width = 210, controlSize = "sm", options = options, selectedIndex = selected, enabled = ready,
      onChange = function(index)
        local id = ENGINES[(tonumber(index) or 0) + 1]
        if id == nil or id == engine then
          return
        end
        -- A model that isn't on disk yet gets downloaded first; pick it again when done.
        C.run({ installed(id) and "stt" or "download", id })
      end,
    })),
    ui.label({ text = noctalia.tr("proc.style"), fontSize = 12, color = "on_surface_variant" }),
  }

  local ids = { "off" }
  for _, m in C.modes(state) do
    table.insert(ids, m.id)
  end
  if #ids <= 4 then
    local buttons = {}
    for _, id in ids do
      table.insert(buttons, ui.button({
        text = C.modeName(state, id), controlSize = "sm", flexGrow = 1, enabled = ready,
        variant = id == mode and "primary" or "ghost", selected = id == mode,
        onClick = function()
          C.run({ "mode", id })
        end,
      }))
    end
    table.insert(rows, ui.row({ gap = 4 }, buttons))
  else
    local names, sel = {}, 0
    for i, id in ids do
      table.insert(names, C.modeName(state, id))
      if id == mode then
        sel = i - 1
      end
    end
    table.insert(rows, ui.select({
      controlSize = "sm", options = names, selectedIndex = sel, enabled = ready,
      onChange = function(index)
        local id = ids[(tonumber(index) or 0) + 1]
        if id then
          C.run({ "mode", id })
        end
      end,
    }))
  end

  if ready and mode ~= "off" then
    local e = state.enhancer
    local line
    if e == "missing" then
      line = ui.row({ gap = 8, align = "center" }, {
        ui.label({ text = noctalia.tr("proc.missing"), fontSize = 11, color = "on_surface_variant", flexGrow = 1 }),
        downloadButton("enhance"),
      })
    else
      local word = e == "ready" and "proc.ready" or (e == "error" and "proc.error" or "proc.loading")
      line = ui.row({ gap = 8, align = "center" }, {
        ui.label({ text = (state.llm or "") .. " · " .. noctalia.tr(word), fontSize = 11, color = e == "error" and "error" or "on_surface_variant", flexGrow = 1 }),
        ui.button({ text = noctalia.tr("proc.edit"), glyph = "pencil", glyphSize = 13, controlSize = "sm", variant = "ghost", onClick = function()
          editPrompt(mode)
        end }),
      })
    end
    table.insert(rows, line)
  end

  if state and state.download then
    local pct = state.download.pct or 0
    table.insert(rows, ui.column({ gap = 4 }, {
      ui.label({ text = noctalia.tr("proc.downloading", { pct = pct }), fontSize = 11, color = "on_surface_variant" }),
      ui.progress({ progress = pct / 100, height = 4, radius = 2 }),
    }))
  end

  table.insert(rows, settingRow(noctalia.tr("proc.context"), ui.toggle({
    checked = state == nil or state.context ~= false,
    enabled = ready,
    onChange = function(v)
      C.run({ "context", v == "true" and "on" or "off" })
    end,
  }), 180))
  return card(rows, "processing")
end

local function footer()
  return ui.row({ gap = 6, align = "center", justify = "center" }, {
    keycap(noctalia.tr("hotkey." .. hotkeyId())),
    ui.label({ text = noctalia.tr("hint_hold") .. "  ·  " .. noctalia.tr("hint_cancel"), fontSize = 11, color = "on_surface_variant" }),
  })
end

function render()
  panel.render(ui.column({ flexGrow = 1, gap = 12, padding = 4 }, {
    header(),
    historyCard(),
    processingCard(),
    settingsCard(),
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
  -- Picks up new or edited mode files.
  C.run({ "modes" })
  panel.setWantsSecondTicks(true)
  refresh()
end

function onClose()
  panel.setWantsSecondTicks(false)
end
