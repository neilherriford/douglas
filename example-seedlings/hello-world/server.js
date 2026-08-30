const http = require('http')
const fs = require('fs')
const path = require('path')

const port = 3000
const publicDir = path.join(__dirname, 'public')

const server = http.createServer((request, response) => {
  const requestedPath = request.url === '/' ? '/index.html' : request.url
  const filePath = path.join(publicDir, requestedPath)

  if (!filePath.startsWith(publicDir)) {
    response.writeHead(403)
    response.end('Forbidden')
    return
  }

  fs.readFile(filePath, (err, contents) => {
    if (err) {
      response.writeHead(404, { 'Content-Type': 'text/plain' })
      response.end('Not found')
      return
    }

    response.writeHead(200, { 'Content-Type': 'text/html' })
    response.end(contents)
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
