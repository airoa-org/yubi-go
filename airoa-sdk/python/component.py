"""Component exports; domain data keeps the upstream JSON schema."""
import json
import traceback
import wit_world
from componentize_py_types import Err
from dispatch import OPS
# componentize-py snapshots imported modules. Load every operation's dependencies
# at build time, including modules only imported lazily by the upstream adapters.
import shutil
import sqlite3
import yaml
import jsonschema
import airoa_core.storage.canonical_path
import airoa_core.storage.upload_state
import airoa_core.storage.config
import airoa_core.storage.gc
import airoa_core.storage.upload
import airoa_core.status.payload
import airoa_core.status.diagnostics
import airoa_core.gate.config
import airoa_core.gate.diagnostics
import airoa_core.gate.expression
import airoa_core.backend.base
import airoa_core.backend.profile
import airoa_core.system_info.aggregator
import airoa_core.system_info.config
import airoa_metadata.schemas
from functools import cache

# Snapshot immutable schema assets before importing MetadataBuilder, which
# captures load_schema by name. No schema files need mounting on the device.
airoa_metadata.schemas.load_schema = cache(airoa_metadata.schemas.load_schema)
for version in airoa_metadata.schemas.AVAILABLE_SCHEMAS:
    airoa_metadata.schemas.load_schema(version)
# RefResolver lazily reads its vocabulary assets on first construction.
jsonschema.RefResolver.from_schema(airoa_metadata.schemas.load_schema("2.0"))
import airoa_core.metadata.builder
# After the schema cache: airoa_core.task imports MetadataBuilder too.
import task_bridge


class WitWorld(wit_world.WitWorld):
    def dispatch(self, operation, arguments):
        try:
            return json.dumps(OPS[operation](json.loads(arguments)), ensure_ascii=False)
        except ValueError as exc:
            raise Err(wit_world.Failure_Refused(str(exc)))
        except Exception:
            raise Err(wit_world.Failure_Python(traceback.format_exc()))
