# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Tokenizer do
  def tokenize(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token.to_a
    end

    tokens
  end

  it "tokenizes new line" do
    _(tokenize("\n")).must_equal [[:"\n"]]
  end

  it "increments line number on new lines" do
    tokenizer = Wid::Tokenizer.new("1\n2\n3\n4")

    _(tokenizer.next_token.line).must_equal 1
    _(tokenizer.next_token.line).must_equal 2
    _(tokenizer.next_token.line).must_equal 2
    _(tokenizer.next_token.line).must_equal 3
    _(tokenizer.next_token.line).must_equal 3
    _(tokenizer.next_token.line).must_equal 4
    _(tokenizer.next_token.line).must_equal 4
  end

  it "returns line number on unrecognized token" do
    tokenizer = Wid::Tokenizer.new("?")

    _ { tokenizer.next_token }.must_raise(RuntimeError, 'Unrecognized token "?" at line 1')
  end

  it "tokenizes numbers" do
    %w[3 2.5].each do |number|
      _(tokenize(number)).must_equal [[:NUMBER, number]]
    end
  end

  it "tokenizes strings" do
    %w['string1' "string2"].each do |string|
      _(tokenize(string)).must_equal [[:STRING, string]]
    end
  end

  it "tokenizes booleans" do
    %w[true false].each do |bool|
      _(tokenize(bool)).must_equal [[:BOOL, bool]]
    end
  end

  it "tokenizes nil" do
    _(tokenize("nil")).must_equal [[:NIL]]
  end

  it "tokenizes literals" do
    %w[( )].each do |operator|
      _(tokenize(operator)).must_equal [[operator.to_sym]]
    end
  end

  it "tokenizes operators" do
    %w[+ - > >= < <= == !=].each do |operator|
      _(tokenize(operator)).must_equal [[operator.to_sym]]
    end
  end

  it "tokenizes identifiers" do
    %w[number puts some_fun __dir__ MY_CONSTANT chomp! empty? value=].each do |identifier|
      _(tokenize(identifier)).must_equal [[:IDENTIFIER, identifier]]
    end
  end
end
