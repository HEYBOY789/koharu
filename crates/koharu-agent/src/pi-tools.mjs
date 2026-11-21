// Loaded only by the Pi process for this editor request.
export default async function (pi) {
  const endpoint = process.env.KOHARU_EDITOR_TOOLS
  const headers = {
    'Content-Type': 'application/json',
    Accept: 'application/json, text/event-stream',
  }
  let sequence = 0

  async function request(method, params, signal, notification = false) {
    const id = notification ? undefined : ++sequence
    const response = await fetch(endpoint, {
      method: 'POST',
      headers,
      body: JSON.stringify({ jsonrpc: '2.0', id, method, params }),
      signal,
    })
    if (!response.ok) throw new Error(`Koharu tool request failed (${response.status})`)
    const session = response.headers.get('Mcp-Session-Id')
    if (session) headers['Mcp-Session-Id'] = session
    if (notification) {
      await response.body?.cancel()
      return
    }
    let message
    if (response.headers.get('Content-Type')?.includes('text/event-stream')) {
      const reader = response.body.pipeThrough(new TextDecoderStream()).getReader()
      let pending = ''
      try {
        while (!message) {
          const chunk = await reader.read()
          if (chunk.done) break
          pending += chunk.value
          const events = pending.split(/\r?\n\r?\n/)
          pending = events.pop()
          for (const event of events) {
            const data = event.split(/\r?\n/)
              .filter((line) => line.startsWith('data:'))
              .map((line) => line.slice(5).trimStart()).join('\n')
            if (!data) continue
            const candidate = JSON.parse(data)
            if (candidate.id === id) message = candidate
          }
        }
      } finally {
        await reader.cancel()
      }
    } else {
      message = await response.json()
    }
    if (!message) throw new Error(`Koharu returned no result for ${method}`)
    if (message.error) throw new Error(message.error.message)
    return message.result
  }

  const server = await request('initialize', {
    protocolVersion: '2025-03-26',
    capabilities: {},
    clientInfo: { name: 'Koharu Pi editor', version: '1' },
  })
  headers['MCP-Protocol-Version'] = server.protocolVersion
  await request('notifications/initialized', {}, undefined, true)
  const { tools } = await request('tools/list', {})
  for (const tool of tools) {
    pi.registerTool({
      name: tool.name,
      label: tool.name,
      description: tool.description ?? tool.name,
      parameters: tool.inputSchema,
      async execute(_id, args, signal) {
        const result = await request('tools/call', { name: tool.name, arguments: args }, signal)
        if (result.isError) throw new Error(result.content.map(item => item.text || '').join('\n'))
        return { content: result.content, details: {} }
      },
    })
  }
}
