const http = require('http')

const port = process.env.PORT
const response_message = process.env.RESPONSE_MESSAGE

const server = http.createServer(function (request, response) {
  response.writeHead(200, {'Content-Type': 'text/plain'})
  response.end(`${response_message}\n`)
})

server.listen(port)

console.log(`Server running at http://localhost: ${port}`)
