#!/bin/sh
exec "${QUALIFICATION_PYTHON:?host must select the trusted interpreter}" -B "$ORACLE_ROOT/check.py"
