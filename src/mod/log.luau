local stdio = require("@lune/stdio")

local function c(text: string, color: stdio.Color)
  return stdio.color(color) .. text .. stdio.color("reset")
end

local log = {}

function log.format(prefix: string, color: stdio.Color, text: string)
  return `{c(prefix, color)} {text}`
end

function log.info(text: string)
  print(log.format("INFO", "blue", text))
end

function log.warn(text: string)
  print(log.format("WARN", "yellow", text))
end

function log.success(text: string)
  print(log.format("OK  ", "green", text))
end

function log.error(text: string)
  print(log.format("ERR ", "red", text))
end

return log
