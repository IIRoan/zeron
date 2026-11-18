#!/usr/bin/env python3
"""Serve the already authenticated Zeron gateway over TLS on one LAN interface."""

import argparse
import asyncio
import ipaddress
import ssl


async def copy(source, destination):
    while chunk := await source.read(65536):
        destination.write(chunk)
        await destination.drain()


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", type=ipaddress.IPv4Address, required=True)
    parser.add_argument("--network", type=ipaddress.IPv4Network, required=True)
    parser.add_argument("--port", type=int, default=27814)
    parser.add_argument("--gateway-port", type=int, default=27812)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    args = parser.parse_args()
    if args.address not in args.network or not args.network.is_private:
        parser.error("the listening address must belong to the private LAN network")

    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.minimum_version = ssl.TLSVersion.TLSv1_2
    tls.load_cert_chain(args.cert, args.key)

    async def handle(reader, writer):
        upstream = None
        try:
            peer = writer.get_extra_info("peername")
            if not peer or ipaddress.ip_address(peer[0]) not in args.network:
                return
            origin, upstream = await asyncio.open_connection("127.0.0.1", args.gateway_port)
            tasks = {
                asyncio.create_task(copy(reader, upstream)),
                asyncio.create_task(copy(origin, writer)),
            }
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
        except OSError:
            pass
        finally:
            if upstream is not None:
                upstream.close()
            writer.close()

    server = await asyncio.start_server(
        handle, str(args.address), args.port, ssl=tls, ssl_handshake_timeout=10
    )
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
