var util = require('util');

module.exports = function (response) {
	response.send = function (status, data) {
		if (typeof status !== 'number') {
			data = status;
			status = undefined;
		}

		if (typeof data === 'object') {
			if (util.isError(data)) {
				status = status || data.status || 500;
				data = data.toString();
			}
			else {
				response.setHeader('Content-Type', 'application/json');
				data = JSON.stringify(data);
			}
		}

		if (status) {
			response.statusCode = status;
		}

		response.end(data);
	};
};