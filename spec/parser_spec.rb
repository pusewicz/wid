# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Parser do
  def parse(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token
    end

    parser = Wid::Parser.new(tokens)
    parser.parse.map(&:to_h)
  end

  it "parses numbers" do
    [3, 2.5].each do |number|
      assert_equal([{type: Wid::AST::Expr::NumberLiteral, value: number}], parse(number.to_s))
    end
  end

  it "parses strings" do
    [%('a string 1'), %("a string 2")].each do |string|
      assert_equal([{
        type: Wid::AST::Expr::StringLiteral,
        value: string[1...-1]
      }], parse(string))
    end
  end

  it "parses booleans" do
    %w[true false].each do |string|
      assert_equal([{
        type: Wid::AST::Expr::BoolLiteral,
        value: string.to_s == "true"
      }], parse(string))
    end
  end

  it "parses nil" do
    assert_equal([{
      type: Wid::AST::Expr::NilLiteral,
      value: nil
    }], parse("nil"))
  end

  # skip "parses binary expressions" do
  #   code = "1 + 2"
  #   expected = [{
  #     type: Wid::AST::Expr::Binary,
  #     left: {
  #       type: Wid::AST::Expr::NumberLiteral,
  #       value: 1
  #     },
  #     operator: "+",
  #     right: {
  #       type: Wid::AST::Expr::NumberLiteral,
  #       value: 2
  #     }
  #   }]
  #   assert_equal(expected, parse(code))
  # end
end
