FROM python:3.12

# Optional arg is the name of the junction python package to install with pip
# this can be either a pypi package name, a wheel URL, or a local path to a
# wheel.
#
# The magic [x] in the default allows the COPY to succeed even if the arg is not set.
ARG junction_wheel=default[x]

WORKDIR /app

COPY ${junction_wheel} .

COPY junction-python/requirements-dev.txt requirements-dev.txt
RUN pip install --upgrade uv
RUN uv pip install --system -r requirements-dev.txt
ADD junction-python/samples/smoke-test/server.py .

# extra stuff just for the client as having it in the same docker image is convenient
RUN if [[ "$junction_wheel" = "default[x]" ]] ; then uv pip install --system junction-python ; else uv pip install --system *.whl ; fi

RUN curl -LO https://storage.googleapis.com/kubernetes-release/release/$(curl -s https://storage.googleapis.com/kubernetes-release/release/stable.txt)/bin/linux/amd64/kubectl
RUN chmod +x ./kubectl
RUN mv ./kubectl /usr/local/bin
ADD junction-python/samples/smoke-test/client.py .
