var use = require('./use');
var url = require('./url');
var send = require('./send');
var call = require('./call');
var done = require('./done');

module.exports = function () {
	var stack = [];

	function router(request, response, after) {
		after = after || done;

		url(request);
		send(response);

		var path = request.path.toLowerCase();
		var index = 0;
		next();

		function next(error) {
			var layer = stack[index++];
			if (!layer) {
				setImmediate(after, error, request, response);
				return;
			}

			var method = layer.method;
			if (method !== '*' && method !== request.method) {
				return next(error);
			}

			var anchor = layer.anchor;
			if (path.substr(0, anchor.length) !== anchor) {
				return next(error);
			}

			var border = path[anchor.length];
			if (border !== undefined && border !== '/' && border !== '.') {
				return next(error);
			}

			request.route = layer.route;
			call(layer.handler, error, request, response, next);
		}
	}

	router.use = use(stack);
	return router;
};