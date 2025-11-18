local fs = require("@lune/fs")
local serde = require("@lune/serde")
local process = require("@lune/process")

local log = require("./log")
local util = require("./util")

local replaceAliases = {}

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
        log.warn(`  Folder '{child.name}' has no files, skipping`)
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

    -- Handle @self alias for init.luau files
    -- @self points to the current module (for init files, this is the parent directory)
    if isInitFile then
      -- For init files, @self represents the module itself
      local selfInstancePath = currentFilePath

      local pattern = `require%([\"']@self/([^\"']+)[\"']%)`

      source = string.gsub(source, pattern, function(subpath)
        modified = true

        -- Build target path: @self/foo means the 'foo' submodule of the current module
        local targetPath = table.clone(selfInstancePath)
        -- Split subpath by "/" and append each part
        for part in string.gmatch(subpath, "[^/]+") do
          table.insert(targetPath, part)
        end

        local relative = table.concat(util.getRelativePath(requireContextPath, targetPath), "/")
        local rep = `require(\"{relative}\")`

        table.insert(repLogs, {
          table.concat(currentFilePath, "/"),
          { `require(\"@self/{subpath}\")`, rep },
        })

        return rep
      end)
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

          local rep

          if nosub then
            rep = `require(\"{relative}\")`
          else
            rep = `require(\"{relative}/{subpath}\")`
          end

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
