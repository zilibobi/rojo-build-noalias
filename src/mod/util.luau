--!nolint LocalShadow
local fs = require("@lune/fs")
local serde = require("@lune/serde")
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
util.VERSION = "1.1.8"

local tempBase = process.env.TEMP or process.env.TMPDIR or "/tmp"
tempBase = string.gsub(tempBase, "\\", "/")
tempBase = string.gsub(tempBase, "/+$", "")

util.TEMP_OUTPUT_FILENAME = tempBase .. "/rojo-build-noalias/build.rbxm"
util.TEMP_WORKSPACE_DIR = tempBase .. "/rojo-build-noalias/workspace"

util.VERBOSE = false
util.PROCESS_ORPHANS = false

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
  local rojoArgs = {}

  -- Filter out tool-only options
  local i = 1
  while i <= #args do
    local arg = args[i]

    -- Skip --convert-to and its argument
    if arg == "--convert-to" then
      i = i + 2 -- skip both --convert-to and the directory argument
      continue
    end

    -- Skip --process-orphans
    if arg == "--process-orphans" then
      i = i + 1
      continue
    end

    -- Add the argument to rojoArgs
    table.insert(rojoArgs, arg)
    i = i + 1
  end

  table.insert(rojoArgs, 1, "build")

  -- Process -o/--output flag
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
    local isInit = string.find(last, "init.luau$") or string.find(last, "init.lua$")

    if isInit then
      table.remove(segments) -- the init file
      table.remove(segments) -- the folder containing the init file
    end

    -- For init files, the folder removal above already accounts for one
    -- depth level, so subtract 1 to avoid over-stripping
    for _ = 1, if isInit then depth - 1 else depth do
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

--[[
  In convert-to mode, the alias replacer computes relative paths based on the
  Rojo instance tree. When the root tree node has a $path (e.g. "src") and a
  child node has its own $path pointing outside that directory (e.g. "pkg"),
  the instance tree treats it as a child of the root, but on the filesystem
  they are siblings. This function moves such directories into the root $path
  directory so the filesystem matches the instance tree, making the computed
  relative requires correct.

  Example:
    Before: src/ and pkg/ are siblings, tree = { "$path": "src", "pkg": { "$path": "pkg" } }
    After:  pkg/ is moved into src/pkg/, tree updated to { "$path": "src", "pkg": { "$path": "src/pkg" } }
]]
function util.flattenTreePaths(workspaceDir: string, projectPath: string)
  local projectSource = fs.readFile(projectPath)
  local project = serde.decode("json", projectSource)

  local tree = project.tree
  if not tree then
    return
  end

  local rootPath = tree["$path"]
  if not rootPath then
    -- Root has no $path (e.g. DataModel projects) — nothing to flatten
    return
  end

  -- Normalize: strip trailing slashes
  rootPath = string.gsub(rootPath, "/+$", "")

  local moved = false

  -- Walk direct children of the tree looking for $path entries
  for key, child in tree do
    if string.sub(key, 1, 1) == "$" then
      continue
    end

    if type(child) ~= "table" or not child["$path"] then
      continue
    end

    local childPath = child["$path"]
    childPath = string.gsub(childPath, "/+$", "")

    -- Check if childPath is already inside rootPath
    if string.find(childPath, "^" .. util.patternSafeString(rootPath) .. "/") or childPath == rootPath then
      continue
    end

    -- This child's $path is outside the root $path — move it inside
    local destPath = `{rootPath}/{key}`
    local absSrc = `{workspaceDir}/{childPath}`
    local absDest = `{workspaceDir}/{destPath}`

    if not fs.isDir(absSrc) then
      -- Could be a single file
      if fs.isFile(absSrc) then
        -- Ensure parent directory exists
        local parentDir = string.gsub(absDest, "/[^/]*$", "")
        if not fs.isDir(parentDir) then
          fs.writeDir(parentDir)
        end
        fs.writeFile(absDest, fs.readFile(absSrc))
        fs.removeFile(absSrc)
      end
    else
      -- Move directory: copy then remove original
      if fs.isDir(absDest) then
        fs.removeDir(absDest)
      end

      util.copyDir(absSrc, absDest)
      fs.removeDir(absSrc)
    end

    -- Update the project file entry
    child["$path"] = destPath
    moved = true
  end

  if moved then
    -- Write updated project file
    local encoded = serde.encode("json", project, true)
    fs.writeFile(projectPath, encoded)
  end
end

return util
