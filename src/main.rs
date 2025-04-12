use ffmpeg_next::{codec, decoder, encoder, format, frame, Dictionary, Packet, Rational};

fn decode_frames(decoder: &mut decoder::Video, encoder: &mut encoder::Video) {
    let mut frame = frame::Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        // let y = frame.data(0);
        // let u = frame.data(1);
        // let v = frame.data(2);
        
        let timestamp = frame.timestamp();
        frame.set_pts(timestamp);
        encoder.send_frame(&frame).unwrap();
    }
}

fn encode_frames(encoder: &mut encoder::Video, out_stream_index: usize, in_time_base: Rational, out_time_base: Rational, out_ctx: &mut format::context::Output) {
    let mut encoded = Packet::empty();
    while encoder.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(out_stream_index);
        encoded.rescale_ts(in_time_base, out_time_base);
        encoded.write_interleaved(out_ctx).unwrap();
    } 
}

fn main() {
    let src = "src.mp4";
    let dst = "dst.mp4";

    // Input
    let mut in_ctx = format::input(&src).unwrap();

    // Finds video stream
    let in_stream = in_ctx.streams().best(ffmpeg_next::media::Type::Video)
        .expect("Could not find a proper video stream");
    let in_stream_index = in_stream.index();
    let in_time_base = in_stream.time_base();

    // Creates decoder
    let decoder_ctx = codec::Context::from_parameters(in_stream.parameters()).unwrap();
    let mut decoder = decoder_ctx.decoder().video().unwrap();

    // Output
    let mut out_ctx = format::output(&dst).unwrap();

    // Creates output stream
    let codec = encoder::find(codec::Id::H264).expect("Couldn't find encoding codec");
    let mut out_stream = out_ctx.add_stream(codec).expect("Couldn't create output stream");
    out_stream.set_time_base(in_time_base);
    let out_stream_index = out_stream.index();

    // Creates encoder
    let mut encoder = codec::context::Context::new_with_codec(codec)
            .encoder().video().unwrap();
    encoder.set_height(decoder.height());
    encoder.set_width(decoder.width());
    encoder.set_aspect_ratio(decoder.aspect_ratio());
    encoder.set_format(decoder.format());
    encoder.set_frame_rate(Some(in_stream.avg_frame_rate()));
    encoder.set_time_base(in_time_base);

    let mut x264_opts = Dictionary::new();
    x264_opts.set("preset", "medium");

    let mut opened_encoder = encoder
        .open_with(x264_opts)
        .expect("error opening x264 with supplied settings");
    out_stream.set_parameters(&opened_encoder);
    
    let out_time_base = out_stream.time_base();
    

    // Parses video
    out_ctx.set_metadata(in_ctx.metadata().to_owned());
    out_ctx.write_header().unwrap();
    for (stream, packet) in in_ctx.packets() {
        if stream.index() == in_stream_index {
            decoder.send_packet(&packet).unwrap();
        }
        decode_frames(&mut decoder, &mut opened_encoder);
        encode_frames(&mut opened_encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
    }
    decoder.send_eof().unwrap();
    decode_frames(&mut decoder, &mut opened_encoder);
    opened_encoder.send_eof().unwrap();
    encode_frames(&mut opened_encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
  
    out_ctx.write_trailer().unwrap();

    // let audio_stream = input_context.streams().best(ffmpeg_next::media::Type::Audio).unwrap();
    // let audio_stream_index = audio_stream.index();
}


// determine pixel format
// rasterize font glyphs into bytes, based on pixel format
// get mut byte data
// filter src --> glyph indexes
// create frame from scratch, adding glyph data
// encode