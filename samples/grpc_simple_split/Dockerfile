FROM python:3.12

ADD jct_grpc_server.py .
ADD helloworld_pb2_grpc.py .
ADD helloworld_pb2.py .
RUN pip install grpcio grpcio-tools
CMD ["python", "./jct_grpc_server.py"]
