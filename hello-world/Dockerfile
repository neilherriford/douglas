FROM node:21.6
WORKDIR /usr/src/app
COPY ./hello-world/package.json ./hello-world/app.js .
RUN npm install
CMD ["npm", "start"]
