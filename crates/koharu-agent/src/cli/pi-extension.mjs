// Explicitly loaded by the editor's Pi process; no global extension is installed.
export default async function (pi) {
  const url = process.env.KOHARU_EDITOR_TOOLS;
  let sequence = 0;
  async function request(method, params, signal) {
    const response = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' },
      body: JSON.stringify({ jsonrpc: '2.0', id: ++sequence, method, params }),
      signal,
    });
    if (!response.ok) throw new Error(`Koharu tool connection closed (${response.status})`);
    const result = await response.json();
    if (result.error) throw new Error(result.error.message);
    return result.result;
  }
  const { tools } = await request('tools/list', {});
  for (const tool of tools) {
    pi.registerTool({
      name: tool.name,
      label: tool.name,
      description: tool.description,
      parameters: tool.inputSchema,
      async execute(_id, args, signal) {
        const result = await request('tools/call', { name: tool.name, arguments: args }, signal);
        if (result.isError) throw new Error(result.content.map(item => item.text || '').join('\n'));
        return { content: result.content, details: {} };
      },
    });
  }
}
