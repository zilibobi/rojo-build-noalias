--!nolint LocalShadow
local fs = require("@lune/fs")
local task = require("@lune/task")
local roblox = require("@lune/roblox")
local process = require("@lune/process")
local stdio = require("@lune/stdio")

local log = require("./log")

export type InstancePath = { string }

export type SourcemapObject = {
  name: string,
  className: string,
  filePaths: { string }?,
  children: { SourcemapObject }?,
}

type SourcemapChildHandler = (obj: SourcemapObject, crumbs: { SourcemapObject }) -> ()

local util = {}

util.NAME = "rojo-build-noalias"
util.VERSION = "1.1.0"
util.TEMP_OUTPUT_FILENAME = "/tmp/rojo-build-noalias/build.rbxm"
util.TEMP_WORKSPACE_DIR = "/tmp/rojo-build-noalias/workspace"

util.VERBOSE = false

-- ensure temp dir exists
local tempDir = string.gsub(util.TEMP_OUTPUT_FILENAME, "/[^/]*$", "")

fs.writeDir(tempDir)
fs.writeDir(util.TEMP_WORKSPACE_DIR)

function util.formatHelp(help: string)
  help = string.gsub(help, "^%s+\n", "")
  help = string.gsub(help, "%s+$", "")

  local lines = string.split(help, "\n")

  local from = 0

  for i, line in lines do
    if i == 1 then
      from = line:match("^ +"):len()
    end

    lines[i] = string.sub(line, from + 1, -1)
  end

  -- first line: name and version
  table.insert(lines, 1, stdio.color("green") .. util.NAME .. stdio.color("reset") .. " " .. util.VERSION)

  local res = string.format(table.concat(lines, "\n"), util.VERSION)

  res = string.gsub(res, "[%u ]+:", stdio.color("yellow") .. "%1" .. stdio.color("reset"))
  res = string.gsub(res, "<%u+>", stdio.color("green") .. "%1" .. stdio.color("reset"))
  res = string.gsub(res, " (%-[^%- ]+),", " " .. stdio.color("green") .. "%1" .. stdio.color("reset") .. ",")
  res = string.gsub(res, "(%-%-[%a%-?]+)", stdio.color("green") .. "%1" .. stdio.color("reset"))

  return res
end

function util.watch(
  process: process.ChildProcess,
  readerName: "stdin" | "stdout" | "stderr" | string,
  callback: (line: string) -> ()
)
  local reader = process[readerName]

  while true do
    local line = reader:read()

    if not line then
      break
    end

    task.spawn(callback, line)
  end
end

function util.processArgs(args: { string }): ({ string }?, string?)
  local output = ""
  local rojoArgs = table.clone(args)

  table.insert(rojoArgs, 1, "build")

  for i, v in rojoArgs do
    if v ~= "-o" and v ~= "--output" then
      continue
    end

    local after = rojoArgs[i + 1]

    if not after then
      log.error("You must specify an output filename for the build with either '-o' or '--output'")
      return
    end

    output = after

    table.remove(rojoArgs, i) -- removes current
    table.remove(rojoArgs, i) -- removes next (i + 1 - 1)

    -- build to our temp directory
    table.insert(rojoArgs, i, "-o")
    table.insert(rojoArgs, i + 1, util.TEMP_OUTPUT_FILENAME)

    break
  end

  return rojoArgs, output
end

local patternUnsafeChars = { "-", "+", "*", "[", "]", "(", ")" }

function util.patternSafeString(str: string)
  for _, rep in patternUnsafeChars do
    str = string.gsub(str, `%{rep}`, `%%{rep}`)
  end

  return str
end

function util.getTimestamp(data: fs.Metadata)
  local ts = data.modifiedAt
  return ts and ts.unixTimestamp or 0
end

function util.tryGetFolderPath(child: SourcemapObject, depth: number?): string?
  local depth = depth or 1
  local currentDepth = depth

  if not child.children then
    return
  end

  local sub

  for _, v in child.children do
    if v.filePaths then
      sub = v
    else
      sub, depth = util.tryGetFolderPath(v, currentDepth + 1)
    end

    if sub then
      break
    end
  end

  if currentDepth ~= 1 then
    return sub, depth
  elseif sub then
    local path = sub.filePaths[1]
    local segments = string.split(path, "/")

    local last = segments[#segments]

    if string.find(last, "init.luau$") or string.find(last, "init.lua") then
      table.remove(segments) -- the init file
      table.remove(segments) -- the folder
      -- both resolve to a single file so we need to remove this 'file'
    end

    for _ = 1, depth do
      table.remove(segments)
    end

    return table.concat(segments, "/")
  end

  return nil
end

function util.getFullInstancePath(obj: roblox.Instance): InstancePath
  local pathSegments = {}
  local prevItem = obj

  while true do
    if prevItem == nil then
      break
    end

    table.insert(pathSegments, prevItem.Name)
    prevItem = prevItem.Parent
  end

  -- reverse order (root -> obj)
  local len = #pathSegments

  for i = len - 1, 1, -1 do
    pathSegments[len] = table.remove(pathSegments, i)
  end

  return pathSegments
end

function util.getFullChildPath(child: SourcemapObject, crumbs: { SourcemapObject }): InstancePath
  local pathSegments = {}

  for _, obj in crumbs do
    table.insert(pathSegments, obj.name)
  end

  return pathSegments
end

function util.getRelativePath(from: InstancePath, to: InstancePath): InstancePath
  local i = 1
  local max = math.min(#from, #to)

  while i <= max and from[i] == to[i] do
    i = i + 1
  end

  local common = i - 1
  local upCount = #from - common - 1

  if upCount < 0 then
    upCount = 0
  end

  local segments = {}

  if upCount == 0 then
    table.insert(segments, ".")
  else
    for _ = 1, upCount do
      table.insert(segments, "..")
    end
  end

  if common + 1 <= #to then
    for j = common + 1, #to do
      table.insert(segments, to[j])
    end
  end

  return segments
end

function util.forEachInChild(child: SourcemapObject, handler: SourcemapChildHandler, crumbs: { SourcemapObject }?)
  local crumbsClone = crumbs and table.clone(crumbs) or {}

  task.spawn(handler, child, crumbsClone)

  if not child.children then
    return
  end

  table.insert(crumbsClone, child)

  for _, child in child.children do
    util.forEachInChild(child, handler, crumbsClone)
  end
end

function util.forEachInSourcemap(sourcemap: SourcemapObject, handler: SourcemapChildHandler)
  if not sourcemap.children then
    return
  end

  for _, child in sourcemap.children do
    util.forEachInChild(child, handler, { sourcemap })
  end
end

function util.copyDir(from: string, to: string, visited: { [string]: boolean }?)
  local visited = visited or {}

  -- detect circular references
  if visited[from] then
    return
  end
  visited[from] = true

  fs.writeDir(to)

  -- directories to skip
  local skipDirs = {
    [".git"] = true,
    ["node_modules"] = true,
    [".luau-lsp"] = true,
    [".vscode"] = true,
  }

  for _, entry in fs.readDir(from) do
    -- skip excluded directories
    if skipDirs[entry] then
      continue
    end

    local fromPath = `{from}/{entry}`
    local toPath = `{to}/{entry}`

    -- check if it's a symlink by comparing metadata
    local ok = pcall(function()
      return fs.metadata(fromPath)
    end)

    -- skip if we can't read metadata or if it's a symlink
    if not ok then
      continue
    end

    if fs.isDir(fromPath) then
      -- recursively copy directory, passing visited table
      util.copyDir(fromPath, toPath, visited)
    else
      -- copy file
      local ok = pcall(function()
        fs.writeFile(toPath, fs.readFile(fromPath))
      end)

      if not ok then
        -- skip files that can't be read
        continue
      end
    end
  end
end

function util.findAllLuaurc(rootDir: string, excludeDir: string?): { string }
  local results = {}

  local function search(dir: string)
    for _, entry in fs.readDir(dir) do
      local path = `{dir}/{entry}`

      -- Skip the exclude directory if specified
      if excludeDir and path == excludeDir then
        continue
      end

      if entry == ".luaurc" and fs.isFile(path) then
        table.insert(results, path)
      elseif fs.isDir(path) and entry ~= "node_modules" and entry ~= ".git" then
        search(path)
      end
    end
  end

  search(rootDir)
  return results
end

function util.findProjectFileForLuaurc(luaurcPath: string): string?
  local dir = string.gsub(luaurcPath, "/[^/]*$", "")

  -- look for common Rojo project files
  local candidates = {
    `{dir}/default.project.json`,
    `{dir}/default.project.jsonc`,
    `{dir}/project.json`,
    `{dir}/project.jsonc`,
  }

  for _, candidate in candidates do
    if fs.isFile(candidate) then
      return candidate
    end
  end

  -- search for any .project.json file
  for _, entry in fs.readDir(dir) do
    if string.find(entry, "%.project%.jsonc?$") then
      return `{dir}/{entry}`
    end
  end

  return nil
end

return util
