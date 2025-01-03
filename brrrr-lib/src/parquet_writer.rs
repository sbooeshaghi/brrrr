// (c) Copyright 2020 Trent Hauck
// All Rights Reserved

use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::result::Result;
use std::sync::Arc;

use flate2::bufread::GzDecoder;
use itertools::Itertools;
use noodles::fasta;
use noodles::fastq;
use noodles::gff;

use arrow::array::*;
use arrow::datatypes::*;
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::errors::BrrrrError;
use crate::types::{FastaRecord, FastqRecord, GffRecord};

#[derive(Debug, Copy, Clone)]
pub enum BioFileCompression {
    UNCOMPRESSED,
    GZIP,
}

/// Converts a GFF file to Parquet.
///
/// # Arguments
/// * `input` The path to the input GFF file.
/// * `output` The path to the output parquet file.
/// * `parquet_compression` The parquet compression to use.
pub fn gff2pq<P: AsRef<Path>>(
    input: P,
    output: P,
    parquet_compression: Compression,
) -> Result<(), BrrrrError> {
    let props = WriterProperties::builder()
        .set_compression(parquet_compression)
        .set_statistics_enabled(true);

    let file_schema = Schema::new(vec![
        Field::new("seqname", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, true),
        Field::new("feature", DataType::Utf8, false),
        Field::new("start", DataType::Int64, false),
        Field::new("end", DataType::Int64, false),
        Field::new("score", DataType::Int64, true),
        Field::new("strand", DataType::Utf8, false),
        Field::new("frame", DataType::Utf8, true),
        Field::new(
            "attribute",
            DataType::Map(
                Box::new(Field::new(
                    "entries",
                    DataType::Struct(vec![
                        Field::new("keys", DataType::Utf8, false),
                        Field::new("values", DataType::Utf8, true),
                    ]),
                    false,
                )),
                false,
            ),
            false,
        ),
    ]);

    let input_file = fs::File::open(input)?;
    let mut reader = gff::Reader::new(BufReader::new(input_file));

    let records = reader.records();

    let file = fs::File::create(output)?;
    let mut writer =
        ArrowWriter::try_new(file, Arc::new(file_schema.clone()), Some(props.build()))?;
    let chunk_size = 2usize.pow(20);

    for chunk in records.into_iter().chunks(chunk_size).into_iter() {
        let mut seqname_builder = StringBuilder::new(2048);
        let mut source_builder = StringBuilder::new(2048);
        let mut feature_builder = StringBuilder::new(2048);
        let mut start_builder = Int64Builder::new(2048);
        let mut end_builder = Int64Builder::new(2048);
        let mut score_builder = Int64Builder::new(2048);
        let mut strand_builder = StringBuilder::new(2048);
        let mut frame_builder = StringBuilder::new(2048);

        let key_builder = StringBuilder::new(2048);
        let value_builder = StringBuilder::new(2048);
        let mut attribute_builder = MapBuilder::new(None, key_builder, value_builder);

        for chunk_i in chunk {
            let record = chunk_i?;

            let gff_type = GffRecord::from(record);

            seqname_builder.append_value(gff_type.seqname)?;
            source_builder.append_value(gff_type.source)?;
            feature_builder.append_value(gff_type.feature)?;
            start_builder.append_value(gff_type.start as i64)?;
            end_builder.append_value(gff_type.end as i64)?;

            match gff_type.score {
                Some(score) => score_builder.append_value(score as i64)?,
                None => score_builder.append_null()?,
            }

            strand_builder.append_value(gff_type.strand)?;

            match gff_type.frame {
                Some(frame) => frame_builder.append_value(frame)?,
                None => frame_builder.append_null()?,
            }

            let record_key_builder = attribute_builder.keys();
            for k in gff_type.attribute.keys() {
                record_key_builder.append_value(k)?;
            }

            let record_value_builder = attribute_builder.values();
            for v in gff_type.attribute.values() {
                record_value_builder.append_value(v)?;
            }

            attribute_builder.append(true)?;
        }

        let seqname_array = seqname_builder.finish();
        let source_array = source_builder.finish();
        let feature_array = feature_builder.finish();
        let start_array = start_builder.finish();
        let end_array = end_builder.finish();
        let score_array = score_builder.finish();
        let strand_array = strand_builder.finish();
        let frame_array = frame_builder.finish();
        let attribute_array = attribute_builder.finish();

        let rb = RecordBatch::try_new(
            Arc::new(file_schema.clone()),
            vec![
                Arc::new(seqname_array),
                Arc::new(source_array),
                Arc::new(feature_array),
                Arc::new(start_array),
                Arc::new(end_array),
                Arc::new(score_array),
                Arc::new(strand_array),
                Arc::new(frame_array),
                Arc::new(attribute_array),
            ],
        )?;

        writer.write(&rb)?;
    }

    writer.close()?;

    Ok(())
}

fn write_records_to_file<P: AsRef<Path>, R: BufRead>(
    mut reader: fasta::Reader<R>,
    output: P,
    parquet_compression: Compression,
) -> Result<(), BrrrrError> {
    let file_schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, true),
        Field::new("sequence", DataType::Utf8, false),
    ]);

    let props = WriterProperties::builder()
        .set_compression(parquet_compression)
        .set_statistics_enabled(true);

    let file = fs::File::create(output)?;
    let mut writer =
        ArrowWriter::try_new(file, Arc::new(file_schema.clone()), Some(props.build()))?;

    let chunk_size = 2usize.pow(20);
    for chunk in reader.records().into_iter().chunks(chunk_size).into_iter() {
        let mut id_builder = Vec::with_capacity(chunk_size);
        let mut description_builder = StringBuilder::new(2048);
        let mut seq_builder = Vec::with_capacity(chunk_size);

        for chunk_i in chunk {
            let record = match chunk_i {
                Ok(r) => FastaRecord::from(r),
                Err(error) => panic!("{}", error),
            };

            id_builder.push(record.id);
            match record.description {
                Some(x) => description_builder
                    .append_value(x)
                    .expect("Couldn't append description."),
                _ => description_builder
                    .append_null()
                    .expect("Couldn't append null description."),
            }
            seq_builder.push(record.sequence);
        }

        let id_array = StringArray::from(id_builder);
        let desc_array = description_builder.finish();
        let seq_array = StringArray::from(seq_builder);

        let rb = RecordBatch::try_new(
            Arc::new(file_schema.clone()),
            vec![
                Arc::new(id_array),
                Arc::new(desc_array),
                Arc::new(seq_array),
            ],
        )?;

        writer.write(&rb)?;
    }

    writer.close()?;
    Ok(())
}

/// Converts a FASTA file to Parquet.
///
/// # Arguments
/// * `input` The the path to the input fasta file.
/// * `output` The the path to the output parquet file.
/// * `parquet_compression` The parquet compression to use.
/// * `bio_file_compression` The compression for the input bio file.
pub fn fa2pq<P: AsRef<Path>>(
    input: &P,
    output: &P,
    parquet_compression: Compression,
    bio_file_compression: BioFileCompression,
) -> Result<(), BrrrrError> {
    match bio_file_compression {
        BioFileCompression::GZIP => {
            let file = fs::File::open(input)?;
            let gz = GzDecoder::new(BufReader::new(file));
            let reader = fasta::Reader::new(BufReader::new(gz));
            write_records_to_file(reader, output, parquet_compression)
        }
        BioFileCompression::UNCOMPRESSED => {
            let file = fs::File::open(input)?;
            let reader = fasta::Reader::new(BufReader::new(file));
            write_records_to_file(reader, output, parquet_compression)
        }
    }
}
/// Converts a FASTQ file to Parquet.
///
/// # Arguments
/// * `input` The path to the input FASTQ file.
/// * `output` The path to the output Parquet file.
/// * `parquet_compression` The Parquet compression to use.
/// * `bio_file_compression` The compression type for the input FASTQ file.
pub fn fq2pq<P: AsRef<Path>>(
    input: P,
    output: P,
    parquet_compression: Compression,
    bio_file_compression: BioFileCompression,
) -> Result<(), BrrrrError> {
    let file_schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("sequence", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, true),
        Field::new("quality", DataType::Utf8, false),
        Field::new("number", DataType::Int64, true),
    ]);

    let props = WriterProperties::builder()
        .set_compression(parquet_compression)
        .set_statistics_enabled(true);

    // Abstract reader for both compressed and uncompressed files
    let reader: Box<dyn std::io::Read> = match bio_file_compression {
        BioFileCompression::GZIP => {
            let file = fs::File::open(input)?;
            let gz = GzDecoder::new(BufReader::new(file));
            Box::new(gz)
        }
        BioFileCompression::UNCOMPRESSED => {
            let file = fs::File::open(input)?;
            Box::new(file)
        }
    };

    let mut fastq_reader = fastq::Reader::new(BufReader::new(reader));
    let records = fastq_reader.records();

    // Write to the Parquet file
    let file = fs::File::create(output)?;
    let mut writer =
        ArrowWriter::try_new(file, Arc::new(file_schema.clone()), Some(props.build()))?;
    let chunk_size = 2usize.pow(20);
    let mut id_builder = StringBuilder::new(2048);
    let mut description_builder = StringBuilder::new(2048);
    let mut seq_builder = StringBuilder::new(2048);
    let mut quality_builder = StringBuilder::new(2048);
    let mut read_number_builder = Int64Builder::new(2048);

    let mut read_number = 0;

    for chunk in records.into_iter().chunks(chunk_size).into_iter() {
        for chunk_i in chunk {
            match chunk_i {
                Ok(record) => {
                    let fastq_record = FastqRecord::from(record);
                    // println!("Processing record: {:?}", fastq_record.id);

                    id_builder.append_value(fastq_record.id)?;
                    match fastq_record.description {
                        Some(x) => description_builder.append_value(x)?,
                        None => description_builder.append_null()?,
                    }
                    seq_builder.append_value(fastq_record.sequence)?;
                    quality_builder.append_value(fastq_record.quality)?;
                    read_number_builder.append_value(read_number)?;
                    read_number += 1;
                }
                Err(e) => {
                    eprintln!("Error reading record: {}", e);
                    return Err(e.into());
                }
            }
        }

        // Check if we have records to process before finalizing the batch
        if id_builder.len() > 0 {
            println!("batch len {}", id_builder.len());
            let id_array = id_builder.finish();
            let desc_array = description_builder.finish();
            let seq_array = seq_builder.finish();
            let quality_array = quality_builder.finish();
            let read_number_array = read_number_builder.finish();
            // print len of each array
            println!(
                "id_array len: {}, desc_array len: {}, seq_array len: {}, quality_array len: {}",
                id_array.len(),
                desc_array.len(),
                seq_array.len(),
                quality_array.len()
            );

            let rb = RecordBatch::try_new(
                Arc::new(file_schema.clone()),
                vec![
                    Arc::new(id_array),
                    Arc::new(seq_array),
                    Arc::new(desc_array),
                    Arc::new(quality_array),
                    Arc::new(read_number_array),
                ],
            )?;

            writer.write(&rb)?;

            // Reset builders for the next chunk
            id_builder = StringBuilder::new(2048);
            description_builder = StringBuilder::new(2048);
            seq_builder = StringBuilder::new(2048);
            quality_builder = StringBuilder::new(2048);
        }
    }

    writer.close()?;
    Ok(())
}
