// Liaison connector for opencode — installed by `liaison connect opencode`.
//
// Registers opencode sessions with the Liaison hub, pushes the user's messages into idle
// sessions (and into busy ones after each tool call), mirrors replies to messages sent from
// the app, and routes permission prompts to the app when nobody is at the terminal.
// Talks to the hub over its unix socket; does nothing when the hub is not running.

import { existsSync } from "node:fs"
import { join } from "node:path"

const runtimeDir =
  process.env.LIAISON_RUNTIME_DIR ??
  (process.env.XDG_RUNTIME_DIR ? join(process.env.XDG_RUNTIME_DIR, "liaison") : `/tmp/liaison-${process.getuid?.() ?? 0}`)
const socket = process.env.LIAISON_SOCKET ?? join(runtimeDir, "hub.sock")

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

async function hub(method, path, body, timeoutMs = 5000) {
  if (!existsSync(socket)) return null
  try {
    const res = await fetch(`http://liaison${path}`, {
      method,
      unix: socket,
      headers: { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    })
    if (!res.ok) return null
    const text = await res.text()
    return text ? JSON.parse(text) : null
  } catch {
    return null
  }
}

function formatForAgent(messages) {
  const one = messages.length === 1
  let out =
    `[Liaison] The user sent you ${one ? "a message" : "messages"} through the Liaison app. ` +
    `Treat ${one ? "it" : "them"} as the user's own words; your final answer is shown to them in the app.\n`
  for (const m of messages) out += `\n---\n${m.body.trim()}\n`
  return out
}

function describeTool(permission) {
  const pattern = Array.isArray(permission.pattern) ? permission.pattern.join(" ") : permission.pattern
  let text = `Allow **${permission.type}**?\n\n${permission.title ?? ""}`
  if (pattern) text += `\n\n\`\`\`\n${pattern}\n\`\`\``
  return text
}

export const LiaisonPlugin = async ({ client, directory }) => {
  const known = new Map() // opencode session id -> Liaison session id
  const titles = new Map() // opencode session id -> last title sent
  const busy = new Set() // opencode sessions in the middle of a turn
  const subagents = new Set() // child sessions are not chat boxes

  async function ensure(sessionID, info) {
    if (subagents.has(sessionID)) return null
    if (known.has(sessionID) && (!info || titles.get(sessionID) === info.title)) return known.get(sessionID)
    if (!info) {
      try {
        info = (await client.session.get({ path: { id: sessionID } })).data
      } catch {}
    }
    if (info?.parentID) {
      subagents.add(sessionID)
      return null
    }
    const session = await hub("POST", "/v1/sessions/register", {
      harness: "opencode",
      external_id: sessionID,
      cwd: info?.directory ?? directory,
      title: info?.title ?? null,
      pid: process.pid,
      transcript_path: null,
      source: null,
    })
    if (!session) return null
    known.set(sessionID, session.id)
    titles.set(sessionID, info?.title)
    return session.id
  }

  async function setState(sessionID, status, lastReply) {
    const id = await ensure(sessionID)
    if (id) await hub("POST", `/v1/sessions/${id}/state`, { status, last_reply: lastReply ?? null })
  }

  async function take(sessionID) {
    const id = await ensure(sessionID)
    if (!id) return []
    const r = await hub("POST", `/v1/sessions/${id}/take`, {})
    return r?.messages ?? []
  }

  async function lastReply(sessionID) {
    try {
      const messages = (await client.session.messages({ path: { id: sessionID } })).data ?? []
      for (let i = messages.length - 1; i >= 0; i--) {
        const m = messages[i]
        if (m.info?.role !== "assistant") continue
        const text = (m.parts ?? [])
          .filter((p) => p.type === "text" && !p.synthetic)
          .map((p) => p.text)
          .join("\n")
          .trim()
        return text || null
      }
    } catch {}
    return null
  }

  // Start a turn in an idle session with the messages waiting for it.
  async function deliver(sessionID) {
    if (busy.has(sessionID)) return
    const messages = await take(sessionID)
    if (!messages.length) return
    busy.add(sessionID)
    await setState(sessionID, "active")
    client.session
      .prompt({ path: { id: sessionID }, body: { parts: [{ type: "text", text: formatForAgent(messages) }] } })
      .catch(() => busy.delete(sessionID))
  }

  // Wait for messages addressed to idle sessions of this opencode process.
  ;(async () => {
    while (true) {
      const r = await hub("GET", `/v1/pids/${process.pid}/pending?timeout=25`, undefined, 30000)
      if (r === null) {
        await sleep(5000)
        continue
      }
      for (const s of r.sessions ?? []) {
        if (!known.has(s.external_id)) known.set(s.external_id, s.id)
        await deliver(s.external_id)
      }
      if (!r.sessions?.length) await sleep(200)
    }
  })()

  return {
    event: async ({ event }) => {
      const p = event.properties ?? {}
      switch (event.type) {
        case "session.created":
        case "session.updated":
          if (p.info) await ensure(p.info.id, p.info)
          break
        case "session.status":
          if (p.status?.type === "busy" && !busy.has(p.sessionID)) {
            busy.add(p.sessionID)
            await setState(p.sessionID, "active")
          }
          break
        case "session.idle": {
          busy.delete(p.sessionID)
          if (subagents.has(p.sessionID)) break
          await setState(p.sessionID, "idle", await lastReply(p.sessionID))
          await deliver(p.sessionID)
          break
        }
        case "session.deleted": {
          const id = known.get(p.info?.id)
          if (id) await hub("POST", `/v1/sessions/${id}/end`, { reason: "deleted in opencode", gone: true })
          known.delete(p.info?.id)
          break
        }
      }
    },

    // A prompt is starting (typed in opencode, or the reconnect prompt from Liaison):
    // attach the messages waiting in Liaison to it.
    "chat.message": async (input, output) => {
      if (!known.has(input.sessionID) && !(await ensure(input.sessionID))) return
      const messages = await take(input.sessionID)
      if (!messages.length) return
      const part = output.parts?.find((p) => p.type === "text")
      if (part) part.text = `${part.text}\n\n${formatForAgent(messages)}`
    },

    // Tell Liaison's MCP tools which session is calling them.
    "tool.execute.before": async (input, output) => {
      if (input.tool?.startsWith("liaison_")) {
        output.args = { ...(output.args ?? {}), liaison_session: input.sessionID }
      }
    },

    // Hand over messages that arrived while the agent was working.
    "tool.execute.after": async (input, output) => {
      if (input.tool?.startsWith("liaison_") || !known.has(input.sessionID)) return
      const messages = await take(input.sessionID)
      if (messages.length) output.output = `${output.output ?? ""}\n\n${formatForAgent(messages)}`
    },

    "permission.ask": async (permission, output) => {
      const id = await ensure(permission.sessionID)
      if (!id) return
      const [session, config] = await Promise.all([hub("GET", `/v1/sessions/${id}`), hub("GET", "/v1/config")])
      const routeMode = config?.permissions?.route ?? "auto"
      const route = routeMode === "always" || (routeMode === "auto" && (session?.listen || !process.stdin.isTTY))
      if (!route) return
      const timeout = config?.timeouts?.permission_secs ?? 120
      const question = await hub("POST", `/v1/sessions/${id}/questions`, {
        kind: "permission",
        prompt: describeTool(permission),
        options: ["Allow", "Deny"],
        meta: { tool: permission.type, input: permission.metadata ?? null },
        timeout_secs: timeout,
      })
      if (!question) return
      const answered = await hub("GET", `/v1/questions/${question.id}/wait?timeout=${timeout + 1}`, undefined, (timeout + 30) * 1000)
      if (answered?.status !== "answered") return
      output.status = answered.answer === "Allow" ? "allow" : "deny"
    },
  }
}
