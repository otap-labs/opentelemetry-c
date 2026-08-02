# SPDX-License-Identifier: Apache-2.0

FROM gcr.io/oss-fuzz-base/base-builder-rust

COPY . $SRC/opentelemetry-c
WORKDIR $SRC/opentelemetry-c
COPY .clusterfuzzlite/build.sh $SRC/build.sh
