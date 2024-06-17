module.exports = function (stack) {
	return function (method, anchor, handler) {
		var route;

		if (typeof method === 'function' || Array.isArray(method)) {
			handler = method;
			method = '*';
			route = '/';
			anchor = '';
		}
		else if (typeof anchor === 'function' || Array.isArray(anchor)) {
			handler = anchor;

			if (method[0] === '/') {
				route = anchor = method;
				method = '*';
			}
			else {
				route = '/';
				anchor = '';
			}
		}
		else {
			route = anchor;
		}

		if (anchor[anchor.length - 1] === '/') {
			anchor = anchor.slice(0, -1);
		}

		if (!Array.isArray(handler)) {
			handler = [handler];

		}

		handler.forEach(function (handler) {
			stack.push({
				anchor: anchor.toLowerCase(),
				method: method.toUpperCase(),
				route: route,
				handler: handler
			});
		});

		return this;
	};
};