class Minio:
    def __init__(self, *args, **kwargs):
        raise RuntimeError(
            "minio is stubbed inside the sandbox: the host performs object storage I/O"
        )
