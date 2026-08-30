const http = require('http')
const vault = require('node-vault')

const port = 3000
const seedlingName = 'secrets'
const agentAddr = process.env.OPENBAO_AGENT_ADDR

async function roundTripThroughOpenbao() {
  const client = vault({ endpoint: agentAddr })

  const message = `hello from openbao, written at ${new Date().toISOString()}`
  await client.write(`kv/data/${seedlingName}/greeting`, { data: { message } })

  const secret = await client.read(`kv/data/${seedlingName}/greeting`)
  return secret.data.data.message
}

const server = http.createServer((request, response) => {
  roundTripThroughOpenbao()
    .then((message) => {
      response.writeHead(200, { 'Content-Type': 'text/plain' })
      response.end(`${message}\n`)
    })
    .catch((err) => {
      response.writeHead(502, { 'Content-Type': 'text/plain' })
      response.end(`Failed to round-trip a secret through OpenBao: ${err.message}\n`)
    })
})

server.listen(port)

console.log(`Server running at http://localhost:${port}`)

process.on('SIGINT', () => {
  console.log('Stopping...')
  server.close(() => {
    console.log('Stopped')
    process.exit(0)
  })
})
