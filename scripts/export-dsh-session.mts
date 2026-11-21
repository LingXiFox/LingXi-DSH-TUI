// Offline only: use the pinned reader without mounting persistence or Agent plugins.
import { constants, createWriteStream } from 'node:fs'
import { lstat } from 'node:fs/promises'
import { once } from 'node:events'
import { basename } from 'node:path'
import { finished } from 'node:stream/promises'
import { readDecodedJsonlSource } from '../runtime/upstream/packages/session/session-persistence-jsonl/src/generation.ts'
import { sessionFormatCatalog } from '../runtime/upstream/packages/session/session-format-catalog/lib/index.js'

async function main() {
  const [source, destination, ...extra] = process.argv.slice(2)
  const version = sessionFormatCatalog.currentVersion
  if (!source || !destination || extra.length || basename(source) !== `session.v${version}.jsonl.zstd`) {
    throw new Error('usage')
  }
  if (!(await lstat(source)).isFile()) throw new Error('not a regular artifact')
  const started = performance.now()
  const cpu = process.cpuUsage()
  const { artifact } = await readDecodedJsonlSource(source, version, 'zstd', {
    createRestore: header => sessionFormatCatalog.createRestore(header, {
      recovery: 'recoverable', validation: 'current',
    }),
  })
  const stats = new Map<string, number>()
  const writer = createWriteStream(destination, {
    flags: constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
    mode: 0o600,
  })
  const done = finished(writer)
  // Keep an early disk error handled while awaiting drain.
  void done.catch(() => {})
  try {
    await once(writer, 'open')
    for (const event of artifact.events) {
      const line = JSON.stringify({
        jsonrpc: '2.0', method: 'session.event', params: { sessionId: artifact.header.id, event },
      }) + '\n'
      if (Buffer.byteLength(line) > 8 * 1024 * 1024) throw new Error('record exceeds Host frame limit')
      if (!writer.write(line)) await once(writer, 'drain')
      stats.set(event.type, (stats.get(event.type) ?? 0) + 1)
    }
    writer.end()
    await done
  } finally {
    writer.destroy()
  }
  const ms = performance.now() - started
  const { user, system } = process.cpuUsage(cpu)
  console.log(JSON.stringify({
    events: artifact.events.length,
    input_bytes: Number(artifact.events.length > 0 ? (await lstat(source)).size : 0),
    elapsed_ms: Math.round(ms), cpu_percent: (user + system) / (10 * ms),
    max_rss_kib: process.resourceUsage().maxRSS,
    user_messages: stats.get('user/message') ?? 0,
    assistant_messages: stats.get('assistant/message') ?? 0,
    tool_calls: stats.get('tool/call') ?? 0,
    tool_results: stats.get('tool/result') ?? 0,
  }))
}

main().catch(() => {
  // Restore errors may contain private session text. Never print them.
  console.error('DSH export failed. Expected a readable current-format artifact and a new output file; details withheld.')
  process.exitCode = 1
})
