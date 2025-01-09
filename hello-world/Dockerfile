FROM node:23.6.0-slim
WORKDIR /usr/src/app
COPY ./package.json ./app.js .
RUN npm install
CMD ["npm", "start"]
