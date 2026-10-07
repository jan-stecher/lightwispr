--!nonstrict
-- Shared helpers for the lightwispr widget and panel. The daemon writes its live state
-- to $XDG_RUNTIME_DIR/lightwispr/state.json; commands go through the `lightwispr` CLI.

local M = {}

local runtime = noctalia.getenv("XDG_RUNTIME_DIR") or "/tmp"
M.statePath = runtime .. "/lightwispr/state.json"
M.historyPath = noctalia.expandPath((noctalia.getenv("XDG_STATE_HOME") or "~/.local/state") .. "/lightwispr/history.json")

-- The CLI may not be on the shell's PATH (~/.local/bin often isn't).
local function bin()
  if noctalia.commandExists("lightwispr") then
    return "lightwispr"
  end
  return noctalia.expandPath("~/.local/bin/lightwispr")
end
M.bin = bin()

function M.readState()
  local raw = noctalia.readFile(M.statePath)
  if raw == nil then
    return nil
  end
  local s = noctalia.json.decode(raw)
  return s
end

function M.readHistory()
  local raw = noctalia.readFile(M.historyPath)
  if raw == nil then
    return {}
  end
  return noctalia.json.decode(raw) or {}
end

-- Fire-and-forget CLI call; `done` (optional) runs after it finished.
function M.run(args, done)
  local argv = { M.bin }
  for _, a in args do
    table.insert(argv, a)
  end
  if done then
    noctalia.runAsync(argv, function(_) done() end, 3000)
  else
    noctalia.runAsync(argv)
  end
end

-- Whether the daemon process is alive (the state file outlives a crash).
function M.checkAlive(cb)
  noctalia.processMatches(cb, "lightwispr", "daemon")
end

function M.ago(unixSeconds)
  local s = os.time() - unixSeconds
  if s < 60 then
    return noctalia.tr("time.now")
  elseif s < 3600 then
    return noctalia.tr("time.min", { n = math.floor(s / 60) })
  elseif s < 86400 then
    return noctalia.tr("time.hour", { n = math.floor(s / 3600) })
  end
  return noctalia.tr("time.day", { n = math.floor(s / 86400) })
end

-- Milliseconds since the last outcome (typed / copied / error …).
function M.sinceLast(state)
  if state == nil or state.last_at == nil or state.last_at == 0 then
    return math.huge
  end
  return noctalia.nowMs() - state.last_at
end

return M
