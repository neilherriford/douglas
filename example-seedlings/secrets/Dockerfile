FROM node:23.6.0-slim
WORKDIR /usr/src/app
COPY ./package.json ./package-lock.json ./
RUN npm ci --omit=dev
COPY ./server.js ./server.js
EXPOSE 3000
CMD ["node", "server.js"]
