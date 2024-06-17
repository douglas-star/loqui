var url = require('url');

module.exports = function (request) {
	var parsed = {};

	if (request.url) {
		parsed = url.parse(request.url, true);
	}

	request.path = parsed.pathname || '/';
	request.query = parsed.query || {};
};