local fs = require("@lune/fs")
local serde = require("@lune/serde")
local process = require("@lune/process")

local log = require("./log")
local util = require("./util")

local replaceAliases = {}

function replaceAliases.inFilesDirectly(workspaceDir: string, luaurcPath: string): ({ { any } } | boolean)?
  -- get luaurc
  local luaurc: {
    aliases: { [string]: string }?,
  }

  do
    local ok, err = pcall(function()
      luaurc = serde.decode("json", fs.readFile(luaurcPath))
    end)

    if not ok then
      log.warn(`Could not open {luaurcPath} with error: {string.split(tostring(err), "\n")[1]}`)
      return false
    end
  end

  local aliases = luaurc.aliases

  if not aliases or next(aliases) == nil then
    if util.VERBOSE then
      log.info(`Skipping {luaurcPath} - no aliases defined`)
    end
    return false
  end

  -- Get the directory containing the .luaurc
  local luaurcDir = string.gsub(luaurcPath, "/[^/]*$", "")

  -- Find all .lua and .luau files in this directory and subdirectories
  local function findLuaFiles(dir: string): { string }
    local results = {}

    local function search(currentDir: string)
      for _, entry in fs.readDir(currentDir) do
        local path = `{currentDir}/{entry}`

        if fs.isDir(path) and entry ~= "node_modules" and entry ~= ".git" then
          search(path)
        elseif string.find(entry, "%.luau?$") then
          table.insert(results, path)
        end
      end
    end

    search(dir)
    return results
  end

  local files = findLuaFiles(luaurcDir)
  local repLogs = {}
  local filesProcessed = 0

  for _, filePath in files do
    local source = fs.readFile(filePath)
    local modified = false
    local relativeFilePath = string.gsub(filePath, "^" .. util.patternSafeString(workspaceDir) .. "/", "")

    for alias, replacement in aliases do
      local patterns = {
        { true, `require%([\"']@{util.patternSafeString(alias)}[\"']%)`, `require(\"{replacement}\")` },
        { false, `require%([\"']@{util.patternSafeString(alias)}/([^\"']+)[\"']%)`, `require(\"{replacement}/%1\")` },
      }

      for _, info in patterns do
        local nosub = info[1]
        local pattern = info[2]
        local replacementPattern = info[3]

        source = string.gsub(source, pattern, function(subpath)
          modified = true

          local originalReq = nosub and `require(\"@{alias}\")` or `require(\"@{alias}/{subpath}\")`
          local newReq = nosub and `require(\"{replacement}\")` or `require(\"{replacement}/{subpath}\")`

          table.insert(repLogs, {
            relativeFilePath,
            { originalReq, newReq },
          })

          return newReq
        end)
      end
    end

    if modified then
      fs.writeFile(filePath, source)
      filesProcessed = filesProcessed + 1
    end
  end

  -- Return the logs for centralized printing
  if #repLogs > 0 then
    return repLogs
  elseif util.VERBOSE then
    log.info(`Processed {luaurcPath} (orphan mode) - checked {#files} files, no alias requires found`)
  end

  return nil
end

-- Helper function to convert self-nested relative paths to @self paths
-- For example: "./ForgeVFX/vfx/bezier" -> "@self/vfx/bezier"
-- This is needed because when Wally downloads a package, it renames the folder
-- to match the package name (e.g., "forge-vfx"), breaking relative requires
-- that use the project name (e.g., "ForgeVFX")
local function convertToSelfPath(relativePath: string, projectRootName: string): string
  local segments = string.split(relativePath, "/")

  -- Check if path starts with "./<ProjectName>/..." pattern
  if #segments >= 2 and segments[1] == "." and segments[2] == projectRootName then
    -- Convert to @self/...
    local selfSegments = { "@self" }
    for i = 3, #segments do
      table.insert(selfSegments, segments[i])
    end
    return table.concat(selfSegments, "/")
  end

  -- Check if path has "../" followed by project root name
  -- e.g., "../ForgeVFX/foo" or "../../ForgeVFX/foo"
  for i, segment in segments do
    if segment == projectRootName and i > 1 then
      -- Check if all previous segments are ".."
      local allParentRefs = true
      for j = 1, i - 1 do
        if segments[j] ~= ".." then
          allParentRefs = false
          break
        end
      end

      if allParentRefs then
        -- Convert to @self/...
        local selfSegments = { "@self" }
        for j = i + 1, #segments do
          table.insert(selfSegments, segments[j])
        end
        return table.concat(selfSegments, "/")
      end
    end
  end

  return relativePath
end

function replaceAliases.inFiles(
  workspaceDir: string,
  luaurcPath: string,
  projectPath: string,
  originalProjectDir: string?
): ({ { any } } | boolean)?
  -- get luaurc
  local luaurc: {
    aliases: { [string]: string }?,
  }

  do
    local ok, err = pcall(function()
      luaurc = serde.decode("json", fs.readFile(luaurcPath))
    end)

    if not ok then
      log.warn(`Could not open {luaurcPath} with error: {string.split(tostring(err), "\n")[1]}`)
      return false
    end
  end

  local aliases = luaurc.aliases

  if not aliases or next(aliases) == nil then
    if util.VERBOSE then
      log.info(`Skipping {luaurcPath} - no aliases defined`)
    end
    return false
  end

  -- get project file
  local project: {
    tree: {
      ["$path"]: string,
    },
  }

  do
    local ok, err = pcall(function()
      project = serde.decode("json", fs.readFile(projectPath))
    end)

    if not ok then
      log.error(`Could not open {projectPath} with error: {string.split(tostring(err), "\n")[1]}`)
      return false
    end
  end

  -- generate sourcemap for this project
  local projectDirPath = string.gsub(projectPath, "/[^/]*$", "")
  local res = process.exec("rojo", { "sourcemap", projectPath }, { cwd = projectDirPath })

  if not res.ok then
    log.error(`Rojo sourcemap generation failed for {projectPath} with exit code {res.code}`)
    return false
  end

  local sourcemap = serde.decode("json", res.stdout)

  -- Get the project root name from the sourcemap (e.g., "ForgeVFX")
  local projectRootName = sourcemap.name

  -- normalize path to be relative to project directory
  local function normalizePath(path: string): string
    -- convert to absolute if relative
    if not string.find(path, "^/") then
      path = `{projectDirPath}/{path}`
    end
    return path
  end

  -- build filePathToNode mapping
  local filePathToNode = {} :: {
    [string]: {
      path: string,
      child: util.SourcemapObject,
      crumbs: { util.SourcemapObject },
    },
  }

  local foldersFound = 0
  local filesFound = 0

  -- Use a custom synchronous iteration instead of util.forEachInSourcemap
  -- because that uses task.spawn which is async
  local function processChild(child: util.SourcemapObject, crumbs: { util.SourcemapObject })
    local paths = {}

    if child.filePaths then
      -- files
      filesFound = filesFound + 1
      table.insert(paths, normalizePath(child.filePaths[1]))
    else
      -- folders - try to find an init file or any file
      foldersFound = foldersFound + 1
      local folderPath = util.tryGetFolderPath(child)
      if folderPath then
        folderPath = normalizePath(folderPath)

        -- store the folder path for alias lookups
        table.insert(paths, folderPath)

        -- also check if there's an init file and store that for file processing
        local initLuau = `{folderPath}/init.luau`
        local initLua = `{folderPath}/init.lua`

        if fs.isFile(initLuau) then
          table.insert(paths, initLuau)
        elseif fs.isFile(initLua) then
          table.insert(paths, initLua)
        end
      else
        log.warn(`Folder '{child.name}' has no files, skipping`)
      end
    end

    if #paths > 0 then
      -- store all paths for this node
      -- for folders, we need to include the folder itself in the crumbs
      -- because getFullChildPath only uses crumbs, not the child
      local crumbsWithChild = table.clone(crumbs)
      table.insert(crumbsWithChild, child)

      for _, path in paths do
        filePathToNode[path] = {
          path = path,
          child = child,
          crumbs = crumbsWithChild,
        }
      end
    end

    -- recursively process children
    if child.children then
      local newCrumbs = table.clone(crumbs)
      table.insert(newCrumbs, child)

      for _, subChild in child.children do
        processChild(subChild, newCrumbs)
      end
    end
  end

  -- Process all children of the sourcemap
  if sourcemap.children then
    for _, child in sourcemap.children do
      processChild(child, { sourcemap })
    end
  end

  if util.VERBOSE then
    log.info(`Sourcemap scan: {filesFound} files, {foldersFound} folders`)
  end

  -- also add the root path of the project to the filePath map
  local rootPath = normalizePath(project.tree["$path"])

  -- store the root folder path for alias lookups
  if not filePathToNode[rootPath] then
    filePathToNode[rootPath] = {
      path = rootPath,
      child = sourcemap,
      crumbs = { sourcemap },
    }
  end

  -- check if root has an init file and store that too
  local rootInitLuau = `{rootPath}/init.luau`
  local rootInitLua = `{rootPath}/init.lua`

  if fs.isFile(rootInitLuau) and not filePathToNode[rootInitLuau] then
    filePathToNode[rootInitLuau] = {
      path = rootInitLuau,
      child = sourcemap,
      crumbs = { sourcemap },
    }
  elseif fs.isFile(rootInitLua) and not filePathToNode[rootInitLua] then
    filePathToNode[rootInitLua] = {
      path = rootInitLua,
      child = sourcemap,
      crumbs = { sourcemap },
    }
  end

  -- build alias to filePath mapping
  local aliasToFilePath: { [string]: string } = {}

  for alias, aliasPath in aliases do
    local path = string.gsub(aliasPath, "^%./", "")
    path = normalizePath(path)

    local node = filePathToNode[path] or filePathToNode[path .. "/init.luau"] or filePathToNode[path .. "/init.lua"]

    if not node then
      log.warn(`Alias '{alias}' with path '{aliasPath}' in {luaurcPath} was not found in the sourcemap.`)
      log.warn(`Looking for: {path}`)
      continue
    end

    if util.VERBOSE then
      -- debug: show what we found
      local instancePath = util.getFullChildPath(node.child, node.crumbs)
      log.info(`Alias '{alias}' -> fs:{node.path} -> instance:{table.concat(instancePath, "/")}`)
    end

    aliasToFilePath[alias] = node.path
  end

  if next(aliasToFilePath) == nil then
    log.warn(`No valid aliases found for {projectPath}`)
    return false
  end

  -- replace aliases in all source files
  local repLogs = {}
  local filesProcessed = 0
  local filesChecked = 0

  for filePath, node in filePathToNode do
    if not (string.find(filePath, "%.luau$") or string.find(filePath, "%.lua$")) then
      continue
    end

    filesChecked = filesChecked + 1

    if not fs.isFile(filePath) then
      log.warn(`File in sourcemap doesn't exist: {filePath}`)
      continue
    end

    local source = fs.readFile(filePath)
    local modified = false
    local currentFilePath = util.getFullChildPath(node.child, node.crumbs)

    -- For init.luau files in Roblox, require resolution is relative to the parent
    -- because the folder's contents become children of the ModuleScript
    local isInitFile = string.find(filePath, "/init%.luau?$")
    local requireContextPath = currentFilePath

    if isInitFile then
      -- In Roblox, for init files, requires resolve relative to the parent level
      requireContextPath = table.clone(currentFilePath)
      table.remove(requireContextPath) -- Remove the module itself to get parent context
    end

    for alias, aliasFilePath in aliasToFilePath do
      local targetNode = filePathToNode[aliasFilePath]
      local patterns = {
        { true, `require%([\"']@{util.patternSafeString(alias)}[\"']%)` },
        { false, `require%([\"']@{util.patternSafeString(alias)}/([^\"']+)[\"']%)` },
      }

      local fullTargetPath: util.InstancePath

      for _, info in patterns do
        local nosub = info[1]
        local pattern = info[2]

        source = string.gsub(source, pattern, function(subpath)
          modified = true

          if not fullTargetPath then
            fullTargetPath = util.getFullChildPath(targetNode.child, targetNode.crumbs)
          end

          -- Use requireContextPath instead of currentFilePath for init files
          local relative = table.concat(util.getRelativePath(requireContextPath, fullTargetPath), "/")

          -- Convert self-nested paths to @self (e.g., "./ForgeVFX/foo" -> "@self/foo")
          -- This is needed because Wally renames packages to match the package name
          if nosub then
            relative = convertToSelfPath(relative, projectRootName)
          else
            relative = convertToSelfPath(`{relative}/{subpath}`, projectRootName)
          end

          local rep = `require(\"{relative}\")`

          table.insert(repLogs, {
            table.concat(currentFilePath, "/"),
            { `require(\"{nosub and `@{alias}` or `@{alias}/{subpath}`}\")`, rep },
          })

          return rep
        end)
      end
    end

    if modified then
      fs.writeFile(filePath, source)
      filesProcessed = filesProcessed + 1
    end
  end

  -- Return the logs for centralized printing
  if #repLogs > 0 then
    return repLogs
  elseif util.VERBOSE then
    log.info(`Processed {projectPath} - checked {filesChecked} files, no alias requires found`)
  end

  return nil
end

return replaceAliases
