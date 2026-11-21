import { createRequire } from 'node:module'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { pathToFileURL } from 'node:url'
import { constants, createWriteStream } from 'node:fs'
import { chmod, copyFile, link, lstat, mkdir, readFile, realpath, rename, symlink, unlink, writeFile } from 'node:fs/promises'
import { once } from 'node:events'
import { finished } from 'node:stream/promises'

const upstream = process.env.DSH_TUI_UPSTREAM
const require = createRequire(join(upstream, 'packages/boot/app-boot/package.json'))
const load = path => import(pathToFileURL(join(upstream, path)).href)
const privateFile = { mode: 0o600, flag: 'wx' }
const importable = new Set(['agent-default-model', 'llm-deepseek', 'llm-pi-ai', 'mcp-manager'])

async function optional(path) {
  try { return await readFile(path, 'utf8') }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}

async function prepare(home, shared) {
  if (await realpath(home) !== resolve(home) || await realpath(shared) === resolve(home)) {
    throw new Error('expected a separate private SDK directory')
  }
  const yaml = require('js-yaml')
  const expression = new yaml.Type('tag:yaml.org,2002:js', {
    kind: 'scalar', construct: () => ({ rejectedExpression: true }),
  })
  const schema = yaml.JSON_SCHEMA.extend(expression)
  const patches = []
  const mcp = { servers: {} }
  for (const path of [join(shared, 'profiles/desktop/cordis.patch.yml'), join(shared, 'cordis.patch.yml')]) {
    const text = await optional(path)
    if (text === undefined) continue
    const rows = yaml.load(text, { schema })
    if (!Array.isArray(rows)) continue
    for (const row of rows) {
      if (!importable.has(row?.id) || !row.config || typeof row.config !== 'object') continue
      const config = JSON.stringify(row.config)
      if (config.includes('"rejectedExpression"')) continue
      if (row.id === 'mcp-manager') {
        for (const [name, server] of Object.entries(row.config.servers ?? {})) {
          if (!server || typeof server !== 'object') continue
          Object.defineProperty(mcp.servers, name, {
            value: process.env.DSH_TUI_ENABLE_MCP === '1' ? server : { ...server, enabled: false },
            enumerable: true, configurable: true,
          })
        }
      } else {
        patches.push({ id: row.id, config: JSON.parse(config) })
      }
    }
  }
  patches.push({ id: 'skill-filesystem', config: { dshHome: shared } },
    { id: 'session-log-deepseek', disabled: true },
    { insert: [
      { id: 'mcp-manager', name: pathToFileURL(join(upstream, 'packages/mcp/mcp-manager/lib/index.js')).href, inject: ['credentials'], config: mcp },
      { id: 'lingxi-host', name: import.meta.url, inject: ['loader', 'llm', 'skills', 'appReady'] },
    ] })
  if (process.getuid) {
    const credentials = join(await realpath(shared), '.credentials.yaml')
    const metadata = await lstat(credentials).catch(error => {
      if (error.code !== 'ENOENT') throw error
    })
    if (metadata?.isFile() && metadata.uid === process.getuid() && (metadata.mode & 0o077) === 0) {
      // The pinned credential provider replaces a destination atomically under a sibling lock.
      await symlink(credentials, join(home, '.credentials.yaml')).catch(error => {
        if (error.code !== 'EEXIST') throw error
      })
    }
  }
  await unlink(join(home, 'lingxi-inventory.json')).catch(error => {
    if (error.code !== 'ENOENT') throw error
  })
  const output = join(home, 'lingxi-overlay.json')
  const pending = `${output}.${process.pid}`
  await writeFile(pending, JSON.stringify(patches), privateFile)
  await rename(pending, output)
}

export function apply(ctx) {
  const home = process.env.DSH_HOME
  const output = join(home, 'lingxi-inventory.json')
  async function refresh() {
    const { readPluginInventory } = await load('packages/host/plugin-inventory/lib/index.js')
    const providers = ctx.llm.listProviders().map(({ id, name }) => ({ id, name, models: [] }))
    const write = async () => {
      const { entries } = await readPluginInventory(ctx)
      const skills = await ctx.skills.list({ cwd: process.cwd() })
      const servers = ctx.get('mcpManager')?.list() ?? []
      const pending = `${output}.${process.pid}`
      await writeFile(pending, JSON.stringify({
        providers,
        plugins: entries.map(({ entryId, moduleName, enabled, fiberPhase }) => ({ id: entryId, name: moduleName, enabled, phase: fiberPhase })),
        skills: skills.map(({ name, source, invocation }) => ({ name, source, userInvocable: invocation.userInvocable })),
        mcp: servers.map(({ name, configured, status }) => ({ name, configured, state: status.state })),
      }), privateFile)
      await rename(pending, output)
    }
    await write()
    await Promise.all(providers.map(async provider => {
      try {
        const models = await ctx.llm.listModels(provider.id)
        provider.models = models.map(({ id, name }) => ({ id, name }))
      } catch { provider.unavailable = true }
    }))
    await write()
  }
  let refreshing = false
  let dirty = false
  function schedule() {
    dirty = true
    if (refreshing) return
    refreshing = true
    void (async () => {
      try {
        while (dirty) {
          dirty = false
          await refresh()
        }
      } catch {
        process.stderr.write('Official DSH inventory unavailable\n')
      } finally {
        refreshing = false
      }
    })()
  }
  ctx.effect(() => ctx.appReady.onReady(schedule))
  ctx.on('mcp-manager/status', schedule)
}

function inside(path, root) {
  const suffix = relative(root, path)
  if (!suffix || isAbsolute(suffix) || suffix === '..' || suffix.startsWith(`..${sep}`)) {
    throw new Error('artifact outside session root')
  }
  return suffix
}

async function history(mode, shared, home, id, journal) {
  const { Context } = await load('vendor/cordis/lib/index.js')
  const persistence = await load('packages/session/session-persistence-jsonl/lib/index.js')
  const root = await realpath(join(shared, 'sessions'))
  const openStore = async root => {
    const ctx = new Context()
    await persistence.apply(ctx, { root })
    return ctx.sessionPersistence
  }
  const store = await openStore(root)
  if (mode === 'history') {
    const rows = (await store.list()).filter(({ header }) => header.origin !== 'subagent')
    rows.sort((a, b) => b.header.createdAt - a.header.createdAt)
    const entries = []
    let remaining = 12 * 1024 * 1024
    const deadline = performance.now() + 2500
    for (const { header, formatStatus, sizeBytes } of rows) {
      const current = formatStatus === 'current'
      let title
      // The persistence header has no title. Consult only small current logs through its read API.
      if (current && sizeBytes > 0 && sizeBytes <= Math.min(remaining, 512 * 1024) && performance.now() < deadline) {
        remaining -= sizeBytes
        try {
          const handle = await store.open(header.id, 'read')
          try {
            const { events } = await handle.read(0, 4096)
            title = events.findLast(event => event.type === 'session/title')?.data.title
            if (!title) {
              const user = events.find(event => event.type === 'user/message' && event.data.source.kind === 'user')
              title = user?.data.content.find(part => part.type === 'text')?.text
            }
          } finally { await handle.close() }
        } catch { /* A busy or unavailable log still has its catalog entry. */ }
      }
      entries.push({ id: header.id, cwd: header.cwd ?? '', date: new Date(header.createdAt).toISOString().slice(0, 10),
        current, title: typeof title === 'string' ? title.trim().split('\n')[0].slice(0, 160) || undefined : undefined })
    }
    process.stdout.write(JSON.stringify(entries))
    return
  }
  const source = await store.stat(id)
  if (source?.formatStatus !== 'current' || source.header.origin === 'subagent' || !source.location?.path) {
    throw new Error('expected a current-format parent session')
  }
  const path = source.location.path
  if (!(await lstat(path)).isFile() || await realpath(path) !== path || !basename(path).startsWith('session.v')) {
    throw new Error('invalid session artifact')
  }
  if (await realpath(home) !== resolve(home) || await realpath(shared) === resolve(home)) {
    throw new Error('expected a separate private SDK directory')
  }
  const privateRoot = join(home, 'sessions')
  const target = join(privateRoot, inside(path, root))
  await mkdir(dirname(target), { recursive: true, mode: 0o700 })
  if (await realpath(dirname(target)) !== resolve(dirname(target))) throw new Error('invalid private directory')
  const privateStore = await openStore(privateRoot)
  if (!await privateStore.stat(id)) {
    const pending = `${target}.${process.pid}`
    await copyFile(path, pending, constants.COPYFILE_EXCL)
    try {
      const latest = await store.stat(id)
      if (latest?.revision !== source.revision) throw new Error('source changed')
      await chmod(pending, 0o600)
      try { await link(pending, target) }
      catch (error) { if (error.code !== 'EEXIST') throw error }
    } finally { await unlink(pending) }
  }
  const snapshot = await privateStore.stat(id)
  if (snapshot?.formatStatus !== 'current' || !snapshot.location?.path
      || await realpath(snapshot.location.path) !== snapshot.location.path) {
    throw new Error('invalid private snapshot')
  }
  inside(snapshot.location.path, privateRoot)
  const handle = await privateStore.open(id, 'read')
  try {
    const writer = createWriteStream(journal, privateFile)
    const done = finished(writer)
    void done.catch(() => {})
    try {
      await once(writer, 'open')
      const { events } = await handle.read()
      for (const event of events) {
        const line = JSON.stringify({ jsonrpc: '2.0', method: 'session.event', params: { sessionId: id, event } }) + '\n'
        if (Buffer.byteLength(line) > 8 * 1024 * 1024) throw new Error('session event exceeds Host limit')
        if (!writer.write(line)) await once(writer, 'drain')
      }
      writer.end()
      await done
    } finally { writer.destroy() }
  } finally { await handle.close() }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [mode, home, shared, id, journal] = process.argv.slice(2)
  const work = mode === 'prepare' ? prepare(home, shared) : history(mode, shared, home, id, journal)
  await work.catch(() => {
    process.stderr.write(`Official DSH ${mode} failed; details withheld\n`)
    process.exitCode = 1
  })
}
