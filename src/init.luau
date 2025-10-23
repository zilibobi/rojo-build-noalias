local fs = require("@lune/fs")
local task = require("@lune/task")
local stdio = require("@lune/stdio")
local serde = require("@lune/serde")
local roblox = require("@lune/roblox")
local process = require("@lune/process")

local log = require("@self/mod/log")
local util = require("@self/mod/util")

if #process.args == 0 or table.find(process.args, "-h") or table.find(process.args, "--help") then
  local help = [[
    A wrapper around the Rojo build command.

    USAGE:
        rojo-build-noalias [OPTIONS] [PROJECT]

    ROJO ARGS:
        <PROJECT>   Path to the project to serve. Defaults to the current directory [default: ]

    ROJO OPTIONS:
            --color <COLOR>     Set color behavior. Valid values are auto, always, and never [default: auto]
        -h, --help              Print help information
        -o, --output <OUTPUT>   Where to output the result
        -v, --verbose           Sets verbosity level. Can be specified multiple times
            --watch             Whether to automatically rebuild when any input files change
    ]]

  print(util.formatHelp(help))

  process.exit()
end

if table.find(process.args, "--plugin") then
  log.error("The '--plugin' option is not currently supported.")
  process.exit()
end

local rojoArgs, rojoOutput = util.processArgs(process.args)

if not rojoArgs or not rojoOutput then
  return
end

local rojo = process.create("rojo", rojoArgs)

-- display rojo errors
task.spawn(util.watch, rojo, "stderr", function(line)
  stdio.write(log.format("ROJO", "purple", line))
end)

-- keep blocking and exit together with rojo
util.watch(rojo, "stdout", function(line)
  stdio.write(log.format("ROJO", "purple", line))

  if not string.find(line, "Built project to") then
    return
  end

  local start = os.clock()

  local ok, res = pcall(function()
    local res = process.exec("rojo", { "sourcemap" })

    if not res.ok then
      log.error(`Rojo sourcemap generation failed with exit code {res.code} and error:\n{res.stderr}`)
      return
    end

    local sourcemap = serde.decode("json", res.stdout)
    local luaurc = serde.decode("json", fs.readFile(".luaurc"))

    local aliases = luaurc.aliases :: { [string]: string }

    if not aliases then
      return
    end

    local filePathToNode = {} :: {
      [string]: {
        path: string,
        child: util.SourcemapObject,
        crumbs: { util.SourcemapObject },
      },
    }

    util.forEachInSourcemap(sourcemap, function(child, crumbs)
      local path

      if child.filePaths then
        -- files
        path = child.filePaths[1]
      else
        -- folders
        path = util.tryGetFolderPath(child)
      end

      if not path then
        log.warn(
          `Folder '{util.getFullChildPath(child, crumbs)}' cannot be mapped because it doesn't have any children!`
        )
        return
      end

      filePathToNode[path] = {
        path = path,
        child = child,
        crumbs = crumbs,
      }
    end)

    local aliasToFilePath: { [string]: string } = {}

    for alias, aliasPath in aliases do
      local path = string.gsub(aliasPath, "^%./", "")
      local node = filePathToNode[path] or filePathToNode[path .. "/init.luau"] or filePathToNode[path .. "/init.lua"]

      if not node then
        log.warn(
          `Alias '{alias}' with path '{aliasPath}' was not found in the sourcemap. Requires using this alias won't be replaced.`
        )
        continue
      end

      aliasToFilePath[alias] = node.path
    end

    local build = roblox.deserializeModel(fs.readFile(util.TEMP_OUTPUT_FILENAME))
    local root = build[1]

    local repLogs = {}

    for _, obj in root:GetDescendants() do
      if not obj:IsA("LuaSourceContainer") then
        continue
      end

      local currentFilePath = util.getFullInstancePath(obj)

      local source = obj.Source
      local modified = false

      for alias, aliasFilePath in aliasToFilePath do
        local node = filePathToNode[aliasFilePath]
        local patterns = {
          { true, `require%(["']@{util.patternSafeString(alias)}["']%)` },
          { false, `require%(["']@{util.patternSafeString(alias)}/([^"']+)["']%)` },
        }

        local fullTargetPath: util.InstancePath

        for _, info in patterns do
          local nosub = info[1]
          local pattern = info[2]

          source = string.gsub(source, pattern, function(subpath)
            modified = true

            if not fullTargetPath then
              fullTargetPath = util.getFullChildPath(node.child, node.crumbs)
            end

            local relative = table.concat(util.getRelativePath(currentFilePath, fullTargetPath), "/")

            local rep

            if nosub then
              rep = `require("{relative}")`
            else
              rep = `require("{relative}/{subpath}")`
            end

            table.insert(repLogs, {
              table.concat(currentFilePath, "/"),
              { `require("{nosub and `@{alias}` or `@{alias}/{subpath}`}")`, rep },
            })

            return rep
          end)
        end
      end

      if modified then
        obj.Source = source
      end
    end

    -- pretty printing
    do
      table.sort(repLogs, function(a, b)
        return #a[2][1] < #b[2][1]
      end)

      local maxLeft = 0
      local maxRight = 0

      for _, info in repLogs do
        maxLeft = math.max(string.len(info[1]), maxLeft)
        maxRight = math.max(string.len(info[2][1]), maxRight)
      end

      for _, info in repLogs do
        log.info(
          info[1]
            .. string.rep(" ", maxLeft - string.len(info[1]))
            .. " "
            .. info[2][1]
            .. string.rep(" ", maxRight - string.len(info[2][1]))
            .. " -> "
            .. info[2][2]
        )
      end
    end

    local model = roblox.serializeModel({ root })
    fs.writeFile(rojoOutput, model)

    log.success(`Built project to {rojoOutput}`)

    return true
  end)

  if ok and res == true then
    local elapsed = math.round((os.clock() - start) * 1000 * 100) / 100
    log.info(`Done in {elapsed}ms\n`)
  elseif not ok then
    log.warn(`Project build failed because an error occurred:\n{res}`)
  end
end)
