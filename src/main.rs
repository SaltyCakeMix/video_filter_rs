use ffmpeg_next::{codec, decoder, encoder, format::{self, Pixel}, frame, picture, software::scaling::{Context, Flags}, Dictionary, Packet, Rational};
use rusttype::{point, Font, Scale};

fn decode_frames(decoder: &mut decoder::Video, encoder: &mut encoder::Video, scaler: &mut Context) {
    let mut frame = frame::Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let mut scaled_frame = frame::Video::empty();
        scaler.run(&frame, &mut scaled_frame).unwrap();
        // let mut y = scaled_frame.data_mut(0);

        let mut new_frame = frame::Video::new(Pixel::YUV420P, 1920, 1080);

        let timestamp = frame.timestamp();
        new_frame.set_pts(timestamp);
        new_frame.set_kind(picture::Type::None);
        encoder.send_frame(&new_frame).unwrap();
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

fn construct_char_set(font_path: &[u8], chars: &str, font_h: u32) -> (u32, Vec<Vec<Vec<u8>>>) {
    // Loads font
    let font = Font::try_from_bytes(font_path).expect("Error constructing Font");

    // Determines proper font scaling
    let func = |height| {
        let scale = Scale::uniform(height);
        let v_metrics = font.v_metrics(scale);
        let glyphs: Vec<_> = font.layout(chars, scale, point(0., v_metrics.ascent)).collect();
        let glyphs_height = glyphs
            .iter()
            .map(|g| g.pixel_bounding_box().unwrap_or_default().height())
            .max()
            .unwrap() as u32;
        (glyphs, glyphs_height)
    };
    let (_, glyphs_height) = func(font_h as f32);
    let adj_factor = font_h as f32 / glyphs_height as f32;

    let (glyphs, glyphs_height) = func(font_h as f32 * adj_factor);
    let glyphs_width = glyphs
        .iter()
        .map(|g| g.pixel_bounding_box().unwrap_or_default().width())
        .max()
        .unwrap() as u32;

    let mut glyph_bytes: Vec<Vec<Vec<u8>>> = Vec::with_capacity(glyphs.len());
    for glyph in glyphs {
        let mut bytes = vec![vec![0; glyphs_width as usize]; glyphs_height as usize];
        if let Some(bounding_box) = glyph.pixel_bounding_box() {
            let x_pad = (glyphs_width - bounding_box.width() as u32) / 2;
            let y_pad = (glyphs_height - bounding_box.height() as u32) / 2;
            glyph.draw(|x, y, v| {
                bytes[(y + y_pad) as usize][(x + x_pad) as usize] = (v * 256.) as u8;
            });
        }
        glyph_bytes.push(bytes);
    }

    (glyphs_width, glyph_bytes)
}

fn main() {
    let src = "src.mp4";
    let dst = "dst.mp4";
    let dst_h = 1080;
    let render_h = 60;
    let font_path = include_bytes!("../MonospaceTypewriter.ttf");
    let char_set = " .-^:~/*+=?%##&$$@@@@@@@@@@@@";

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
    if decoder.format() != Pixel::YUV420P {
        panic!("Pixel format is not YUV420P");
    };

    // Relevant data
    let src_w = decoder.width();
    let src_h = decoder.height();
    let dst_w = dst_h * src_w / src_h;
    let font_h = dst_h / render_h;
    let src_fmt = decoder.format();
    
    // Font
    let (font_w, char_set) = construct_char_set(font_path, char_set, font_h);
    let font_aspect_ratio = font_w as f32 / font_h as f32;
    let render_w = ((render_h * src_w / src_h) as f32 / font_aspect_ratio) as u32;

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
    encoder.set_width(dst_w);
    encoder.set_height(dst_h);
    encoder.set_aspect_ratio(decoder.aspect_ratio());
    encoder.set_format(src_fmt);
    encoder.set_frame_rate(Some(in_stream.avg_frame_rate()));
    encoder.set_time_base(in_time_base);

    let mut x264_opts = Dictionary::new();
    x264_opts.set("preset", "medium");

    let mut opened_encoder = encoder
        .open_with(x264_opts)
        .expect("error opening x264 with supplied settings");
    out_stream.set_parameters(&opened_encoder);
    
    let dst_fmt = opened_encoder.format();
    let out_time_base = out_stream.time_base();


    // Parses video
    // let mut scaler = Context::get(
    //     src_fmt,
    //     src_w, src_h,
    //     dst_fmt,
    //     render_w, render_h,
    //     Flags::FAST_BILINEAR,
    // ).unwrap();

    // out_ctx.set_metadata(in_ctx.metadata().to_owned());
    // out_ctx.write_header().unwrap();
    // for (stream, packet) in in_ctx.packets() {
    //     if stream.index() == in_stream_index {
    //         decoder.send_packet(&packet).unwrap();
    //     }
    //     decode_frames(&mut decoder, &mut opened_encoder, &mut scaler);
    //     encode_frames(&mut opened_encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
    // }
    // decoder.send_eof().unwrap();
    // decode_frames(&mut decoder, &mut opened_encoder, &mut scaler);
    // opened_encoder.send_eof().unwrap();
    // encode_frames(&mut opened_encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
  
    // out_ctx.write_trailer().unwrap();

    // let audio_stream = input_context.streams().best(ffmpeg_next::media::Type::Audio).unwrap();
    // let audio_stream_index = audio_stream.index();
}


// determine pixel format
// rasterize font glyphs into bytes, based on pixel format
// get mut byte data
// filter src --> glyph indexes
// create frame from scratch, adding glyph data
// encode
//fix frame rate issue