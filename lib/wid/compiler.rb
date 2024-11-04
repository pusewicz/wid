module Wid
  class Compiler
    def self.compile(input, ir: :c)
      tokens = Tokenizer.tokenize(input)
      ast = Parser.parse(tokens)
      case ir
      when :c
        Codegen::C.generate(ast)
      else
        raise "Unsupported IR: #{ir}"
      end
    end
  end
end
